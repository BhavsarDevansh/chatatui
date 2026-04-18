use crate::client::ChatClient;
use crate::mcp::McpTool;
use ratatui::widgets::ListState;
use tokio::sync::mpsc;
use serde_json::json;

#[derive(Debug)]
pub enum AsyncEvent {
    ListModels,
    ChatChunk(String),
    ChatError(String),
    ChatFinished,
    ModelsLoaded(Vec<String>),
    LoadMcpTools(usize), // server index
    McpToolsLoaded(String, Vec<McpTool>),
    McpToolsError(String),
    McpToolsStartupLoaded(String, Vec<McpTool>, crate::mcp::McpClient), // (server_name, tools, client)
    ToolCallRequested(PendingToolCall),
    ToolCallResult { id: String, name: String, content: String },
}

#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub id: String,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub server_name: String,
    pub args_display: String,
}

#[derive(Clone)]
pub struct Thread {
    pub name: String,
    pub messages: Vec<(String, String, bool)>, // (role, content, is_error)
}

#[derive(Debug, Clone)]
pub enum Modal {
    SelectModel(Vec<String>, usize, String), // (models, selected_index, search_filter)
    CommandList(Vec<(String, String)>, usize, String), // (commands with descriptions, selected_index, partial_input)
    McpServers(Vec<(String, String)>, usize), // (server names and urls, selected_index)
    McpTools(String, Vec<McpTool>, usize), // (server_name, tools, selected_index)
    ToolConfirm(String, String), // (tool_name_with_server, args_display)
}

const AVAILABLE_COMMANDS: &[(&str, &str)] = &[
    ("/new", "Create new thread"),
    ("/model", "Select or switch model"),
    ("/mcp", "List MCP servers and tools"),
    ("/copy", "Copy last message to clipboard"),
    ("/help", "Show available commands"),
];

pub struct AppState {
    pub input: String,
    pub threads: Vec<Thread>,
    pub current_thread_idx: usize,
    pub client: ChatClient,
    pub model: String,
    pub api_name: String,
    pub is_loading: bool,
    pub is_connected: bool,
    pub thread_state: ListState,
    pub chat_scroll: u16,
    pub message_queue: Vec<String>,
    pub modal: Option<Modal>,
    pub mcp_servers: Vec<crate::config::McpServerConfig>,
    pub mcp_tools: Vec<McpTool>,
    pub mcp_clients: Vec<(String, crate::mcp::McpClient)>,
    pub pending_tool_call: Option<PendingToolCall>,
}

fn extract_api_name(url: &str) -> String {
    url.trim_end_matches('/')
        .split("://")
        .last()
        .unwrap_or("API")
        .split('.')
        .next()
        .unwrap_or("API")
        .chars()
        .map(|c| if c.is_alphabetic() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .next()
        .unwrap_or("API")
        .chars()
        .enumerate()
        .map(|(i, c)| if i == 0 { c.to_uppercase().next().unwrap() } else { c })
        .collect()
}

fn get_matching_commands(partial: &str) -> Vec<(String, String)> {
    AVAILABLE_COMMANDS
        .iter()
        .filter(|(cmd, _)| cmd.starts_with(partial))
        .map(|(cmd, desc)| (cmd.to_string(), desc.to_string()))
        .collect()
}

fn get_autocomplete_suggestion(input: &str) -> Option<String> {
    if !input.starts_with('/') {
        return None;
    }

    let matches = get_matching_commands(input);
    if matches.len() == 1 {
        Some(matches[0].0.clone())
    } else {
        None
    }
}

impl AppState {
    pub fn new(url: String, model: String, api_key: Option<String>, mcp_servers: Vec<crate::config::McpServerConfig>) -> Self {
        let mut thread_state = ListState::default();
        thread_state.select(Some(0));

        let client = ChatClient::new(url.clone(), model.clone(), api_key);
        let api_name = extract_api_name(&url);

        Self {
            input: String::new(),
            threads: vec![Thread {
                name: "Default Chat".to_string(),
                messages: vec![],
            }],
            current_thread_idx: 0,
            client,
            model,
            api_name,
            is_loading: false,
            is_connected: true,
            thread_state,
            chat_scroll: 0,
            message_queue: Vec::new(),
            modal: None,
            mcp_servers,
            mcp_tools: Vec::new(),
            mcp_clients: Vec::new(),
            pending_tool_call: None,
        }
    }

    pub fn current_thread(&mut self) -> &mut Thread {
        &mut self.threads[self.current_thread_idx]
    }

    pub fn try_open_command_modal(&mut self) {
        if self.input.starts_with('/') {
            let commands = get_matching_commands(&self.input);
            if !commands.is_empty() && !self.input.contains(' ') {
                self.modal = Some(Modal::CommandList(commands, 0, self.input.clone()));
            }
        }
    }

    pub fn close_command_modal(&mut self) {
        if matches!(self.modal, Some(Modal::CommandList(_, _, _))) {
            self.modal = None;
        }
    }

    pub fn accept_autocomplete(&mut self) {
        if let Some(suggestion) = self.get_autocomplete_suggestion() {
            self.input = suggestion;
            self.close_command_modal();
        }
    }

    pub fn get_autocomplete_suggestion(&self) -> Option<String> {
        get_autocomplete_suggestion(&self.input)
    }

    pub async fn send_message(&mut self, tx: mpsc::Sender<AsyncEvent>) {
        if self.input.trim().is_empty() {
            return;
        }

        self.close_command_modal();

        let input_clone = self.input.clone();
        let trimmed_input = input_clone.trim();

        if trimmed_input.starts_with('/') {
            self.handle_command(trimmed_input, tx).await;
            self.input.clear();
            return;
        }

        if self.is_loading {
            self.message_queue.push(self.input.clone());
            self.input.clear();
            return;
        }

        let user_msg = self.input.clone();
        let model_name = self.model.clone();
        self.current_thread()
            .messages
            .push(("User".to_string(), user_msg, false));
        self.input.clear();
        self.is_loading = true;

        self.current_thread().messages.push((
            "System".to_string(),
            format!("{} is thinking...", model_name),
            false,
        ));

        let history: Vec<(String, String)> = self
            .current_thread()
            .messages
            .iter()
            .filter(|(role, _, _)| role == "User" || role == "Assistant" || role == "Tool" || role == "AssistantToolCall")
            .map(|(role, content, _)| {
                let (api_role, api_content) = match role.as_str() {
                    "User" => ("user".to_string(), content.clone()),
                    "Assistant" => ("assistant".to_string(), content.clone()),
                    "AssistantToolCall" => ("assistant".to_string(), content.clone()),
                    "Tool" => {
                        // Parse "id||name||content" format
                        let parts: Vec<&str> = content.splitn(3, "||").collect();
                        if parts.len() == 3 {
                            ("tool".to_string(), parts[2].to_string())
                        } else {
                            ("tool".to_string(), content.clone())
                        }
                    }
                    _ => ("user".to_string(), content.clone()),
                };
                (api_role, api_content)
            })
            .collect();

        let tools: Vec<serde_json::Value> = self.mcp_tools.iter().map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }
            })
        }).collect();

        self.client.send_chat_message(history, tools, tx).await;
    }

    pub async fn send_next_queued_message(&mut self, tx: mpsc::Sender<AsyncEvent>) {
        if !self.message_queue.is_empty() {
            let msg = self.message_queue.remove(0);
            self.input = msg;
            self.send_message(tx).await;
        }
    }

    pub async fn re_prompt_for_tool_call(&mut self, tx: mpsc::Sender<AsyncEvent>) {
        if self.is_loading {
            return;
        }

        self.is_loading = true;
        let model_name = self.model.clone();
        self.current_thread().messages.push((
            "System".to_string(),
            format!("{} is thinking...", model_name),
            false,
        ));

        let history: Vec<(String, String)> = self
            .current_thread()
            .messages
            .iter()
            .filter(|(role, _, _)| role == "User" || role == "Assistant" || role == "Tool" || role == "AssistantToolCall")
            .map(|(role, content, _)| {
                let (api_role, api_content) = match role.as_str() {
                    "User" => ("user".to_string(), content.clone()),
                    "Assistant" => ("assistant".to_string(), content.clone()),
                    "AssistantToolCall" => ("assistant".to_string(), content.clone()),
                    "Tool" => {
                        let parts: Vec<&str> = content.splitn(3, "||").collect();
                        if parts.len() == 3 {
                            ("tool".to_string(), parts[2].to_string())
                        } else {
                            ("tool".to_string(), content.clone())
                        }
                    }
                    _ => ("user".to_string(), content.clone()),
                };
                (api_role, api_content)
            })
            .collect();

        let tools: Vec<serde_json::Value> = self.mcp_tools.iter().map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema
                }
            })
        }).collect();

        self.client.send_chat_message(history, tools, tx).await;
    }

    async fn handle_command(&mut self, cmd: &str, tx: mpsc::Sender<AsyncEvent>) {
        let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
        let command = parts[0];
        let arg = parts.get(1).map(|s| s.to_string()).unwrap_or_default();

        match command {
            "/new" => {
                self.create_thread(arg.clone());
                let thread_name = self.threads[self.current_thread_idx].name.clone();
                self.current_thread().messages.push((
                    "System".to_string(),
                    format!("Created thread: {}", thread_name),
                    false,
                ));
            }
            "/model" => {
                let _ = tx.send(AsyncEvent::ListModels).await;
            }
            "/mcp" => {
                let servers: Vec<(String, String)> = self
                    .mcp_servers
                    .iter()
                    .map(|s| (s.name.clone(), s.url.clone()))
                    .collect();
                if servers.is_empty() {
                    self.current_thread().messages.push((
                        "System".to_string(),
                        "No MCP servers configured.".to_string(),
                        false,
                    ));
                } else {
                    self.modal = Some(Modal::McpServers(servers, 0));
                }
            }
            "/copy" => {
                self.copy_last_message();
            }
            "/help" => {
                let mut help_text = "Available commands:\n".to_string();
                for (cmd, desc) in AVAILABLE_COMMANDS {
                    help_text.push_str(&format!("{} - {}\n", cmd, desc));
                }
                self.current_thread().messages.push((
                    "System".to_string(),
                    help_text,
                    false,
                ));
            }
            _ => {
                self.current_thread().messages.push((
                    "System".to_string(),
                    format!("Unknown command: {}", command),
                    false,
                ));
            }
        }
    }

    pub fn modal_input(&mut self, c: char) {
        match &mut self.modal {
            Some(Modal::SelectModel(_, idx, search)) => {
                search.push(c);
                *idx = 0;
            }
            Some(Modal::CommandList(_, _, partial)) => {
                partial.push(c);
                let new_partial = partial.clone();
                let commands = get_matching_commands(&new_partial);
                if commands.is_empty() {
                    self.modal = None;
                } else {
                    self.modal = Some(Modal::CommandList(commands, 0, new_partial));
                }
            }
            _ => {}
        }
    }

    pub fn modal_backspace(&mut self) {
        match &mut self.modal {
            Some(Modal::SelectModel(_, idx, search)) => {
                search.pop();
                *idx = 0;
            }
            Some(Modal::CommandList(_, _, partial)) => {
                partial.pop();
                let commands = get_matching_commands(partial);
                if commands.is_empty() {
                    self.modal = None;
                } else {
                    self.modal = Some(Modal::CommandList(commands, 0, partial.clone()));
                }
            }
            _ => {}
        }
    }

    pub fn modal_select_up(&mut self) {
        if let Some(Modal::SelectModel(_, ref mut idx, _)) = self.modal {
            if *idx > 0 {
                *idx -= 1;
            }
        } else if let Some(Modal::CommandList(_, ref mut idx, _)) = self.modal {
            if *idx > 0 {
                *idx -= 1;
            }
        } else if let Some(Modal::McpServers(_, ref mut idx)) = self.modal {
            if *idx > 0 {
                *idx -= 1;
            }
        } else if let Some(Modal::McpTools(_, _, ref mut idx)) = self.modal {
            if *idx > 0 {
                *idx -= 1;
            }
        } else if let Some(Modal::ToolConfirm(_, _)) = self.modal {
        }
    }

    pub fn modal_select_down(&mut self, max: usize) {
        if let Some(Modal::SelectModel(models, idx, search)) = &mut self.modal {
            let filtered_count = models
                .iter()
                .filter(|m| m.to_lowercase().contains(&search.to_lowercase()))
                .count();
            if *idx < filtered_count.saturating_sub(1) {
                *idx += 1;
            }
        } else if let Some(Modal::CommandList(_, idx, _)) = &mut self.modal {
            if *idx < max.saturating_sub(1) {
                *idx += 1;
            }
        } else if let Some(Modal::McpServers(servers, idx)) = &mut self.modal {
            if *idx < servers.len().saturating_sub(1) {
                *idx += 1;
            }
        } else if let Some(Modal::McpTools(_, tools, idx)) = &mut self.modal {
            if *idx < tools.len().saturating_sub(1) {
                *idx += 1;
            }
        } else if let Some(Modal::ToolConfirm(_, _)) = &mut self.modal {
        }
    }

    pub fn modal_confirm(&mut self) -> Option<usize> {
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::SelectModel(models, idx, search) => {
                    let filtered: Vec<_> = models
                        .iter()
                        .filter(|m| m.to_lowercase().contains(&search.to_lowercase()))
                        .collect();
                    if idx < filtered.len() {
                        self.switch_model(filtered[idx].clone());
                    }
                    None
                }
                Modal::CommandList(commands, idx, _) => {
                    if idx < commands.len() {
                        self.input = commands[idx].0.clone();
                    }
                    None
                }
                Modal::McpServers(_servers, idx) => {
                    Some(idx)
                }
                Modal::McpTools(_, _, _) => {
                    None
                }
                Modal::ToolConfirm(_, _) => {
                    None
                }
            }
        } else {
            None
        }
    }

    pub fn open_models_modal(&mut self, models: Vec<String>) {
        self.modal = Some(Modal::SelectModel(models, 0, String::new()));
    }

    pub fn next_thread(&mut self) {
        self.current_thread_idx = (self.current_thread_idx + 1) % self.threads.len();
        self.thread_state.select(Some(self.current_thread_idx));
    }

    pub fn prev_thread(&mut self) {
        if self.current_thread_idx == 0 {
            self.current_thread_idx = self.threads.len() - 1;
        } else {
            self.current_thread_idx -= 1;
        }
        self.thread_state.select(Some(self.current_thread_idx));
        self.chat_scroll = 0;
    }

    pub fn scroll_up(&mut self) {
        self.chat_scroll = self.chat_scroll.saturating_sub(3);
    }

    pub fn scroll_down(&mut self) {
        self.chat_scroll = self.chat_scroll.saturating_add(3);
    }

    pub fn create_thread(&mut self, name: String) {
        let thread_name = if name.trim().is_empty() {
            format!("Chat {}", self.threads.len() + 1)
        } else {
            name
        };
        self.threads.push(Thread {
            name: thread_name,
            messages: vec![],
        });
        self.current_thread_idx = self.threads.len() - 1;
        self.thread_state.select(Some(self.current_thread_idx));
        self.chat_scroll = 0;
    }

    pub fn switch_model(&mut self, model_name: String) {
        self.model = model_name.clone();
        self.client.set_model(model_name);
        self.current_thread()
            .messages
            .push(("System".to_string(), "Model switched.".to_string(), false));
    }

    fn copy_last_message(&mut self) {
        let messages = &self.current_thread().messages;
        if messages.is_empty() {
            self.current_thread().messages.push((
                "System".to_string(),
                "No messages to copy.".to_string(),
                false,
            ));
            return;
        }

        let mut last_user_idx = None;
        for (idx, (role, _, _)) in messages.iter().enumerate().rev() {
            if role == "User" {
                last_user_idx = Some(idx);
                break;
            }
        }

        let content_to_copy = if let Some(user_idx) = last_user_idx {
            let mut text = String::new();
            for (role, content, _) in &messages[user_idx + 1..] {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("{}: {}", role, content));
            }
            if text.is_empty() {
                "No messages after last user input.".to_string()
            } else {
                text
            }
        } else {
            messages
                .iter()
                .map(|(role, content, _)| format!("{}: {}", role, content))
                .collect::<Vec<_>>()
                .join("\n")
        };

        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                match clipboard.set_text(content_to_copy) {
                    Ok(_) => {
                        self.current_thread().messages.push((
                            "System".to_string(),
                            "Copied to clipboard.".to_string(),
                            false,
                        ));
                    }
                    Err(_) => {
                        self.current_thread().messages.push((
                            "System".to_string(),
                            "Failed to copy to clipboard.".to_string(),
                            true,
                        ));
                    }
                }
            }
            Err(_) => {
                self.current_thread().messages.push((
                    "System".to_string(),
                    "Clipboard not available.".to_string(),
                    true,
                ));
            }
        }
    }

}
