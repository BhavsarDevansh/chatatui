use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;
use serde_json::json;

mod app;
mod client;
mod config;
mod mcp;
mod ui;

use app::{AppState, AsyncEvent};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "A TUI for chatting with OpenAI-compatible APIs"
)]
struct Args {
    #[arg(short, long)]
    url: Option<String>,

    #[arg(short, long)]
    model: Option<String>,
}

async fn run_app(args: Args) -> io::Result<()> {
    enable_raw_mode()?;

    let mut terminal = ui::init_terminal()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;

    let config = config::Config::load();

    let url = args.url.unwrap_or(config.api_url);
    let model = args.model.unwrap_or(config.default_model);
    let api_key = config.api_key;
    let mcp_servers = config.mcp_servers.clone();

    let mut app_state = AppState::new(url, model, api_key, mcp_servers);
    let (tx, mut rx) = mpsc::channel(100);

    // Auto-load MCP tools at startup
    for (_idx, server_config) in app_state.mcp_servers.clone().iter().enumerate() {
        let server = server_config.clone();
        let tx_clone = tx.clone();
        tokio::spawn(async move {
            let client = crate::mcp::McpClient::new(
                server.name.clone(),
                server.url.clone(),
                server.api_key.clone(),
            );
            match client.list_tools().await {
                Ok(tools) => {
                    let _ = tx_clone.send(AsyncEvent::McpToolsStartupLoaded(server.name.clone(), tools, client)).await;
                }
                Err(e) => {
                    let _ = tx_clone
                        .send(AsyncEvent::ChatError(format!("Failed to load MCP tools from {}: {}", server.name, e)))
                        .await;
                }
            }
        });
    }

    ui::draw(&mut terminal, &mut app_state)?;

    let result = loop {
        let mut needs_redraw = false;

        if event::poll(Duration::from_millis(50))? {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('c')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            break Ok(());
                        }
                        KeyCode::Enter => {
                            if let Some(crate::app::Modal::CommandList(_, _, _)) = &app_state.modal {
                                app_state.modal_confirm();
                                app_state.send_message(tx.clone()).await;
                            } else if app_state.modal.is_some() {
                                if let Some(server_idx) = app_state.modal_confirm() {
                                    let _ = tx.send(AsyncEvent::LoadMcpTools(server_idx)).await;
                                }
                            } else {
                                app_state.send_message(tx.clone()).await;
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Char(c) => {
                            if let Some(crate::app::Modal::ToolConfirm(_, _)) = &app_state.modal {
                                match c {
                                    'y' => {
                                        if let Some(pending) = app_state.pending_tool_call.take() {
                                            app_state.modal = None;
                                            let client_idx = app_state.mcp_clients.iter().position(|(name, _)| name == &pending.server_name);
                                            if let Some(idx) = client_idx {
                                                let (_, client) = app_state.mcp_clients[idx].clone();
                                                let tool_name = pending.tool_name.clone();
                                                let args = pending.args.clone();
                                                let tool_id = pending.id.clone();
                                                let tx_clone = tx.clone();
                                                tokio::spawn(async move {
                                                    match client.call_tool(&tool_name, args).await {
                                                        Ok(result) => {
                                                            let _ = tx_clone.send(AsyncEvent::ToolCallResult {
                                                                id: tool_id,
                                                                name: tool_name,
                                                                content: result,
                                                            }).await;
                                                        }
                                                        Err(e) => {
                                                            let _ = tx_clone
                                                                .send(AsyncEvent::ChatError(format!("Tool call failed: {}", e)))
                                                                .await;
                                                        }
                                                    }
                                                });
                                            }
                                        }
                                    }
                                    'n' => {
                                        app_state.modal = None;
                                        app_state.pending_tool_call = None;
                                        app_state.current_thread().messages.push((
                                            "System".to_string(),
                                            "Tool call cancelled by user.".to_string(),
                                            false,
                                        ));
                                    }
                                    _ => {}
                                }
                            } else if app_state.modal.is_some() {
                                app_state.modal_input(c);
                            } else {
                                app_state.input.push(c);
                                app_state.try_open_command_modal();
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Backspace => {
                            if app_state.modal.is_some() {
                                app_state.modal_backspace();
                            } else {
                                app_state.input.pop();
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Up => {
                            match &app_state.modal {
                                Some(crate::app::Modal::SelectModel(_, _, _))
                                | Some(crate::app::Modal::CommandList(_, _, _))
                                | Some(crate::app::Modal::McpServers(_, _))
                                | Some(crate::app::Modal::McpTools(_, _, _)) => {
                                    app_state.modal_select_up();
                                }
                                Some(crate::app::Modal::ToolConfirm(_, _)) | None => {
                                    app_state.scroll_up();
                                }
                            }
                            needs_redraw = true;
                        }
                        KeyCode::Down => {
                            match &app_state.modal {
                                Some(crate::app::Modal::SelectModel(models, _, search)) => {
                                    let filtered_count = models
                                        .iter()
                                        .filter(|m| m.to_lowercase().contains(&search.to_lowercase()))
                                        .count();
                                    app_state.modal_select_down(filtered_count);
                                }
                                Some(crate::app::Modal::CommandList(commands, _, _)) => {
                                    app_state.modal_select_down(commands.len());
                                }
                                Some(crate::app::Modal::McpServers(servers, _)) => {
                                    app_state.modal_select_down(servers.len());
                                }
                                Some(crate::app::Modal::McpTools(_, tools, _)) => {
                                    app_state.modal_select_down(tools.len());
                                }
                                Some(crate::app::Modal::ToolConfirm(_, _)) | None => {
                                    app_state.scroll_down();
                                }
                            }
                            needs_redraw = true;
                        }
                        KeyCode::PageUp => {
                            app_state.prev_thread();
                            needs_redraw = true;
                        }
                        KeyCode::PageDown => {
                            app_state.next_thread();
                            needs_redraw = true;
                        }
                        KeyCode::Esc => {
                            app_state.modal = None;
                            needs_redraw = true;
                        }
                        KeyCode::Tab => {
                            if let Some(crate::app::Modal::CommandList(_, _, _)) = &app_state.modal {
                                app_state.modal_confirm();
                            } else {
                                app_state.accept_autocomplete();
                            }
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                }
            }
        }

        loop {
            match rx.try_recv() {
                Ok(event) => {
                    match event {
                        AsyncEvent::ChatChunk(text) => {
                            let thread = app_state.current_thread();

                            if let Some((role, _, _)) = thread.messages.last() {
                                if role == "System" {
                                    thread.messages.pop();
                                }
                            }

                            if thread
                                .messages
                                .last()
                                .map(|(r, _, _)| r)
                                == Some(&"User".to_string())
                            {
                                thread.messages.push((
                                    "Assistant".to_string(),
                                    text,
                                    false,
                                ));
                            } else if let Some((role, content, is_error)) =
                                thread.messages.last_mut()
                            {
                                if role == "Assistant" && !*is_error {
                                    content.push_str(&text);
                                }
                            }
                            needs_redraw = true;
                        }
                        AsyncEvent::ChatError(text) => {
                            app_state
                                .current_thread()
                                .messages
                                .push(("Error".to_string(), text, true));
                            needs_redraw = true;
                        }
                        AsyncEvent::ChatFinished => {
                            app_state.is_loading = false;
                            if !app_state.message_queue.is_empty() {
                                app_state.send_next_queued_message(tx.clone()).await;
                            }
                            needs_redraw = true;
                        }
                        AsyncEvent::ListModels => {
                            let client = app_state.client.clone();
                            let tx_clone = tx.clone();
                            tokio::spawn(async move {
                                match client.list_models().await {
                                    Ok(models) => {
                                        let _ = tx_clone.send(AsyncEvent::ModelsLoaded(models)).await;
                                    }
                                    Err(e) => {
                                        let _ = tx_clone
                                            .send(AsyncEvent::ChatError(format!("Failed to load models: {}", e)))
                                            .await;
                                    }
                                }
                            });
                        }
                        AsyncEvent::ModelsLoaded(models) => {
                            app_state.open_models_modal(models);
                            needs_redraw = true;
                        }
                        AsyncEvent::LoadMcpTools(server_idx) => {
                            if server_idx < app_state.mcp_servers.len() {
                                let server = app_state.mcp_servers[server_idx].clone();
                                let client = crate::mcp::McpClient::new(
                                    server.name.clone(),
                                    server.url.clone(),
                                    server.api_key.clone(),
                                );
                                let server_name = server.name.clone();
                                let tx_clone = tx.clone();
                                tokio::spawn(async move {
                                    match client.list_tools().await {
                                        Ok(tools) => {
                                            let _ = tx_clone
                                                .send(AsyncEvent::McpToolsLoaded(server_name, tools))
                                                .await;
                                        }
                                        Err(e) => {
                                            let _ = tx_clone
                                                .send(AsyncEvent::McpToolsError(format!("Failed to load tools: {}", e)))
                                                .await;
                                        }
                                    }
                                });
                            }
                        }
                        AsyncEvent::McpToolsLoaded(server_name, tools) => {
                            app_state.modal = Some(crate::app::Modal::McpTools(server_name, tools, 0));
                            needs_redraw = true;
                        }
                        AsyncEvent::McpToolsError(error) => {
                            app_state.modal = None;
                            app_state.current_thread().messages.push((
                                "System".to_string(),
                                format!("MCP Error: {}", error),
                                true,
                            ));
                            needs_redraw = true;
                        }
                        AsyncEvent::McpToolsStartupLoaded(server_name, tools, client) => {
                            app_state.mcp_clients.push((server_name, client));
                            app_state.mcp_tools.extend(tools);
                            needs_redraw = false;
                        }
                        AsyncEvent::ToolCallRequested(mut pending) => {
                            // Find the server that has this tool
                            if let Some(tool) = app_state.mcp_tools.iter().find(|t| t.name == pending.tool_name) {
                                pending.server_name = tool.server_name.clone();
                            }

                            // Store the assistant message with tool_calls
                            let tool_call_json = json!({
                                "id": pending.id,
                                "type": "function",
                                "function": {
                                    "name": pending.tool_name,
                                    "arguments": pending.args
                                }
                            });
                            let assistant_msg = json!({
                                "role": "assistant",
                                "tool_calls": [tool_call_json]
                            });
                            app_state.current_thread().messages.push((
                                "AssistantToolCall".to_string(),
                                assistant_msg.to_string(),
                                false,
                            ));

                            app_state.pending_tool_call = Some(pending.clone());
                            app_state.modal = Some(crate::app::Modal::ToolConfirm(
                                format!("{} ({})", pending.tool_name, pending.server_name),
                                pending.args_display.clone(),
                            ));
                            needs_redraw = true;
                        }
                        AsyncEvent::ToolCallResult { id, name, content } => {
                            // Store tool result in thread
                            app_state.current_thread().messages.push((
                                "Tool".to_string(),
                                format!("{}||{}||{}", id, name, content),
                                false,
                            ));

                            // Re-prompt the LLM with the tool result
                            app_state.re_prompt_for_tool_call(tx.clone()).await;
                            needs_redraw = true;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        if needs_redraw {
            ui::draw(&mut terminal, &mut app_state)?;
        }
    };

    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let args = Args::parse();
    run_app(args).await
}
