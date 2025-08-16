# Warp SQLite MCP Server

A Rust-based MCP (Model Context Protocol) server that provides SQLite access to Warp terminal's database, enabling AI agents to query conversation history, manage notebooks, and interact with Warp's data.

## Features

- **SQLite CRUD Operations**: Generic insert, select, update, and delete operations
- **Notebook Management**: Create, read, update, delete, and list Warp notebooks
- **MCP Server Management**: Register/unregister MCP servers and manage their environment variables
- **Conversation History Access**: Query AI conversations, commands, and agent interactions

## Prerequisites

- Rust (latest stable version)
- Warp terminal installed
- Access to Warp's SQLite database

## Installation

1. Clone the repository:
```bash
git clone https://github.com/samuelatagana/warp-sqlite-mcp.git
cd warp-sqlite-mcp
```

2. Build the project:
```bash
cargo build --release
```

The binary will be available at `target/release/warp-sqlite-mcp`

## Configuration

The server can be configured in three ways (in order of precedence):

1. **Environment Variable**:
```bash
export DATABASE_URL="sqlite:///Users/samuelatagana/Library/Application Support/dev.warp.Warp-Stable/warp.sqlite"
```

2. **Config File** (`config.toml` in the project directory):
```toml
[database]
url = "sqlite:///Users/samuelatagana/Library/Application Support/dev.warp.Warp-Stable/warp.sqlite"
```

3. **Default**: Falls back to `sqlite://./app.sqlite` if no configuration is provided

## Usage

### Running the Server

```bash
# Using environment variable
DATABASE_URL="sqlite:///path/to/warp.sqlite" ./target/release/warp-sqlite-mcp

# Using config file
./target/release/warp-sqlite-mcp
```

### Available Tools

#### Generic SQLite Operations
- `sqlite_insert` - Insert a row into any table
- `sqlite_select` - Query rows from any table
- `sqlite_update` - Update rows in any table
- `sqlite_delete` - Delete rows from any table

#### Notebook Management
- `notebook_create` - Create a new notebook
- `notebook_list` - List notebooks with optional search
- `notebook_get` - Get a specific notebook by ID
- `notebook_append` - Append text to an existing notebook
- `notebook_delete` - Delete a notebook

#### MCP Server Management
- `mcp_register_server` - Register an MCP server
- `mcp_unregister_server` - Unregister an MCP server
- `mcp_set_env` - Set environment variables for an MCP server
- `mcp_get_env` - Get environment variables for an MCP server

## Known Issues

- **Type Conversion Bug**: Currently, there's an issue where JSON numbers are being deserialized as floats instead of integers, causing errors with tools that expect `i64` parameters. This affects:
  - Tools with `limit` or `offset` parameters
  - Tools with `id` parameters (notebook_get, notebook_append, notebook_delete)
  
  **Workaround**: Use SQL queries directly via `sqlite_select` without numeric parameters.

## Database Schema

The Warp database contains numerous tables including:
- `ai_queries` - AI conversation queries
- `agent_conversations` - Agent mode conversations  
- `notebooks` - Warp notebooks
- `active_mcp_servers` - Registered MCP servers
- `commands` - Command history
- And many more...

## Development

### Running Tests
```bash
cargo test
```

### Building for Development
```bash
cargo build
```

### Code Structure
- `src/main.rs` - Main server implementation
- Uses `rmcp` v0.5.0 for MCP protocol implementation
- Uses `sqlx` for SQLite database access
- Async runtime powered by `tokio`

## Contributing

Issues and pull requests are welcome! Please ensure:
1. Code follows Rust conventions
2. All tests pass
3. New features include appropriate documentation

## License

[Add your license here]

## Author

Samuel Atagana
