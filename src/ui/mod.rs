use crate::app::{AppState, Modal};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};
use std::io;

pub fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    let backend = CrosstermBackend::new(io::stdout());
    Terminal::new(backend)
}

pub fn draw(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app_state: &mut AppState,
) -> io::Result<()> {
    terminal.draw(|f| {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
            .split(f.area());

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(chunks[0]);

        let threads_items: Vec<ListItem> = app_state
            .threads
            .iter()
            .enumerate()
            .map(|(idx, t)| {
                let prefix = if idx == app_state.current_thread_idx { "▶ " } else { "  " };
                ListItem::new(format!("{}{}", prefix, t.name))
            })
            .collect();
        let threads_list = List::new(threads_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Threads (↑↓) "),
        );

        f.render_stateful_widget(threads_list, left_chunks[0], &mut app_state.thread_state);

        let status_color = if app_state.is_connected {
            Color::Green
        } else {
            Color::Red
        };
        let status_text = format!("● {}", app_state.api_name);
        let status_widget = Paragraph::new(status_text)
            .style(Style::new().fg(status_color))
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(status_widget, left_chunks[1]);

        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(chunks[1]);

        let mut chat_lines: Vec<Line> = Vec::new();
        for (role, content, is_error) in &app_state.current_thread().messages {
            let prefix = if role == "User" {
                "You: "
            } else if role == "Error" {
                "Error: "
            } else if role == "System" {
                "[System] "
            } else {
                "AI: "
            };

            let style = if *is_error {
                Style::new().fg(Color::Red)
            } else if role == "User" {
                Style::new().fg(Color::Cyan)
            } else if role == "System" {
                Style::new().fg(Color::Gray)
            } else {
                Style::new()
            };

            // Add prefix as first span
            let mut spans = vec![Span::styled(prefix.to_string(), style)];

            // Add content, preserving newlines
            for (i, line) in content.lines().enumerate() {
                if i > 0 {
                    // New line, start fresh
                    chat_lines.push(Line::from(spans.clone()));
                    spans = vec![Span::styled("  ".to_string(), style)]; // Indent continuation
                }
                spans.push(Span::styled(line.to_string(), style));
            }

            // Add the final line
            if !spans.is_empty() {
                chat_lines.push(Line::from(spans));
            }

            // Add spacing between messages
            chat_lines.push(Line::from(""));
        }

        let chat_title = format!(
            " {} | Model: {} ",
            app_state.threads[app_state.current_thread_idx].name, app_state.model
        );

        let chat_widget = Paragraph::new(chat_lines)
            .block(Block::default().borders(Borders::ALL).title(chat_title))
            .wrap(Wrap { trim: false })
            .scroll((app_state.chat_scroll, 0));

        f.render_widget(chat_widget, main_chunks[0]);

        let input_text = if app_state.input.starts_with('/') {
            if let Some(suggestion) = app_state.get_autocomplete_suggestion() {
                let remaining = suggestion[app_state.input.len()..].to_string();
                format!("{} {}{}", ">>", app_state.input, remaining)
            } else {
                format!("{} {}", ">>", app_state.input)
            }
        } else {
            format!("{} {}", ">>", app_state.input)
        };

        let input_widget = Paragraph::new(input_text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Input (↑↓ scroll | PgUp/PgDn threads | Enter send | Ctrl+C quit) "),
        );

        f.render_widget(input_widget, main_chunks[1]);

        if let Some(modal) = &app_state.modal {
            render_modal_background(f, f.area());
            render_modal(f, modal, f.area());
        }
    })?;

    Ok(())
}

fn render_modal_background(f: &mut ratatui::Frame, area: Rect) {
    let background = Paragraph::new("")
        .style(Style::new().bg(Color::Black).fg(Color::DarkGray));
    f.render_widget(background, area);
}

fn render_modal(f: &mut ratatui::Frame, modal: &Modal, area: Rect) {
    let modal_width = 50;
    let max_modal_height = 16;
    let modal_height = match modal {
        Modal::SelectModel(models, _, search) => {
            let filtered_count = models
                .iter()
                .filter(|m| m.to_lowercase().contains(&search.to_lowercase()))
                .count();
            ((filtered_count as u16) + 4).min(max_modal_height)
        }
        Modal::CommandList(commands, _, _) => ((commands.len() as u16) + 3).min(max_modal_height),
        Modal::McpServers(servers, _) => ((servers.len() as u16) + 3).min(max_modal_height),
        Modal::McpTools(_, tools, _) => ((tools.len() as u16) + 3).min(max_modal_height),
        Modal::ToolConfirm(_, _) => 8,
    };

    let x = (area.width.saturating_sub(modal_width)) / 2;
    let y = (area.height.saturating_sub(modal_height)) / 2;
    let modal_area = Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    };

    match modal {
        Modal::SelectModel(models, selected, search) => {
            let filtered: Vec<&String> = models
                .iter()
                .filter(|m| m.to_lowercase().contains(&search.to_lowercase()))
                .collect();

            let visible_height = (modal_area.height.saturating_sub(2)) as usize;
            let scroll = calculate_scroll(*selected, filtered.len(), visible_height);

            let items: Vec<ListItem> = filtered
                .iter()
                .skip(scroll)
                .take(visible_height)
                .enumerate()
                .map(|(i, model)| {
                    let idx = scroll + i;
                    let prefix = if idx == *selected { "▶ " } else { "  " };
                    ListItem::new(format!("{}{}", prefix, model))
                })
                .collect();

            let title = if search.is_empty() {
                " Select Model (type to filter, ↑↓ ↵ Esc) ".to_string()
            } else {
                format!(" {} ({} match) ", search, filtered.len())
            };

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(title)
                        .title_alignment(Alignment::Center)
                        .style(Style::new().bg(Color::DarkGray).fg(Color::White)),
                )
                .style(Style::new().bg(Color::DarkGray).fg(Color::White));

            f.render_widget(list, modal_area);
        }
        Modal::CommandList(commands, selected, partial) => {
            let visible_height = (modal_area.height.saturating_sub(2)) as usize;
            let scroll = calculate_scroll(*selected, commands.len(), visible_height);

            let items: Vec<ListItem> = commands
                .iter()
                .skip(scroll)
                .take(visible_height)
                .enumerate()
                .map(|(i, (cmd, desc))| {
                    let idx = scroll + i;
                    let prefix = if idx == *selected { "▶ " } else { "  " };
                    ListItem::new(format!("{}{} - {}", prefix, cmd, desc))
                })
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" Commands matching '{}' (↑↓↵Tab Esc) ", partial))
                        .title_alignment(Alignment::Center)
                        .style(Style::new().bg(Color::DarkGray).fg(Color::White)),
                )
                .style(Style::new().bg(Color::DarkGray).fg(Color::White));

            f.render_widget(list, modal_area);
        }
        Modal::McpServers(servers, selected) => {
            let visible_height = (modal_area.height.saturating_sub(2)) as usize;
            let scroll = calculate_scroll(*selected, servers.len(), visible_height);

            let items: Vec<ListItem> = servers
                .iter()
                .skip(scroll)
                .take(visible_height)
                .enumerate()
                .map(|(i, (name, url))| {
                    let idx = scroll + i;
                    let prefix = if idx == *selected { "▶ " } else { "  " };
                    ListItem::new(format!("{}{} ({})", prefix, name, url))
                })
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" MCP Servers (↑↓ ↵ Esc) ")
                        .title_alignment(Alignment::Center)
                        .style(Style::new().bg(Color::DarkGray).fg(Color::White)),
                )
                .style(Style::new().bg(Color::DarkGray).fg(Color::White));

            f.render_widget(list, modal_area);
        }
        Modal::McpTools(server_name, tools, selected) => {
            let visible_height = (modal_area.height.saturating_sub(2)) as usize;
            let scroll = calculate_scroll(*selected, tools.len(), visible_height);

            let items: Vec<ListItem> = tools
                .iter()
                .skip(scroll)
                .take(visible_height)
                .enumerate()
                .map(|(i, tool)| {
                    let idx = scroll + i;
                    let prefix = if idx == *selected { "▶ " } else { "  " };
                    ListItem::new(format!("{}{}", prefix, tool.title))
                })
                .collect();

            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" {} Tools (↑↓ Esc) ", server_name))
                        .title_alignment(Alignment::Center)
                        .style(Style::new().bg(Color::DarkGray).fg(Color::White)),
                )
                .style(Style::new().bg(Color::DarkGray).fg(Color::White));

            f.render_widget(list, modal_area);
        }
        Modal::ToolConfirm(tool_name, args) => {
            let content = format!("Tool: {}\n\nArgs:\n{}\n\n(y=confirm, n=cancel)", tool_name, args);
            let paragraph = Paragraph::new(content)
                .block(Block::default()
                    .borders(Borders::ALL)
                    .title(" Call tool? (y/n) ")
                    .title_alignment(Alignment::Center)
                    .style(Style::new().bg(Color::DarkGray).fg(Color::White)))
                .style(Style::new().bg(Color::DarkGray).fg(Color::White));

            f.render_widget(paragraph, modal_area);
        }
    }
}

fn calculate_scroll(selected: usize, total: usize, visible: usize) -> usize {
    if visible >= total || visible == 0 {
        return 0;
    }

    let target_top = selected.saturating_sub(visible / 5);
    let target_bottom = selected.saturating_sub(visible.saturating_sub(1).saturating_sub(visible / 5));

    target_top.min(target_bottom).min(total.saturating_sub(visible))
}
