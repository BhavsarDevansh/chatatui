# chatatui

A terminal UI for chatting with OpenAI-compatible APIs, with support for Model Context Protocol (MCP) tools.

## Features

- Interactive TUI for chat conversations
- Support for OpenAI-compatible APIs
- Multiple conversation threads
- Model selection
- MCP server integration with automatic tool loading
- LLM-driven tool calling with user confirmation
- Slash commands for common operations

## Building

```bash
cargo build --release
```

## Configuration

Create a `config.toml` file in the project directory:

```toml
api_url = "http://localhost:8000"
default_model = "gpt-3.5-turbo"
api_key = "your-api-key"

[[mcp_servers]]
name = "example-server"
url = "http://localhost:3000"
api_key = "optional-key"
```

## Running

```bash
cargo run -- --url http://localhost:8000 --model gpt-3.5-turbo
```

Or use the defaults from `config.toml`:

```bash
cargo run
```

## Usage

- **Enter**: Send message
- **Ctrl+C**: Exit
- **Up/Down**: Scroll chat or navigate menus
- **Page Up/Down**: Switch threads
- **/**: Open command menu

### Commands

- `/new [name]` - Create new thread
- `/model` - Select model
- `/mcp` - List MCP servers and tools
- `/copy` - Copy last message to clipboard
- `/help` - Show help

## License

MIT
