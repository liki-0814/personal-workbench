# Personal Workbench (`pwcli` & Web UI)

Personal Workbench is a powerful, workspace-centric agent runtime designed for developers and AI pair programming. It combines a robust Rust-based command-line interface (`pwcli`) with a gorgeous React-based web workbench to run, monitor, and configure agent tasks with high fidelity, control, and visibility.

---

## Architecture Overview

```text
User Input / Web UI
  ├──> Context Builder
  └──> Tool Selection
        └──> ToolRegistrySnapshot
              └──> Graph Run (Workflow Steps)
                    ├──> Policy Guard (Allow / Deny / AskUser)
                    ├──> Tool Execution (Builtin, MCP, Executable Skills)
                    └──> Audit Record (~/.pwcli/audit/events.jsonl)
```

For a deep dive into the design and layout of the runtime boundary and configuration details, see:
* [Architecture Documentation](docs/architecture.md)
* [Configuration Guide](docs/configuration.md)

---

## Core Features

### 1. Unified Capability Layer (Tools)
`pwcli` abstracts all capabilities as tools. A tool can come from:
* **Builtin Code**: Direct Rust-implemented utilities.
* **External Agent Skills**: Dynamically discovered directories containing a `SKILL.md` manifest and optional script execution.
* **Model Providers**: Built-in clients for OpenAI, Anthropic, and NVIDIA (used internally by graph nodes and planners).
* **MCP (Model Context Protocol)**: Seamless connection to external MCP servers.

### 2. Policy and Audit Logs
Every single tool execution is guarded by a configurable `PolicyGuard` that yields:
* `Allow`: Runs the tool immediately.
* `Deny`: Rejects execution and reports to the model.
* `AskUser`: Prompts the user interactively (TUI or Web UI) for explicit approval before running.
All actions and inputs are appended to the JSONL audit trail at `~/.pwcli/audit/events.jsonl`.

### 3. TUI & Interactive CLI
The terminal interface supports rich commands, inline help, auto-suggestions, and step-by-step goal execution:
* `/goal <objective>`: Starts a goal-directed run.
* `/providers` / `/models`: Configures and switches AI backends.
* `/thinking on|off`: Toggles model reasoning capabilities.
* `/audit`: Displays execution logs and summaries.

### 4. Interactive Web Workbench
A Vite + React frontend (`web/`) designed with premium Codex aesthetics:
* **Collapsible Drawer**: Search, categorize, and organize historical sessions.
* **Workflow Strip**: Real-time progress tracker (`Plan` -> `Execute` -> `Verify` -> `Review`).
* **Settings & Memory Management**: Directly inspect registered tools, system prompts, memory cards, and provider configurations.

---

## Getting Started

### Prerequisites
* **Rust**: `rustc` and `cargo` installed.
* **Node.js**: `node` and `npm` installed (for the Web UI).

### Setup and Initialization
1. **Initialize the Workbench configuration**:
   ```bash
   cargo run -- init
   ```
   This initializes `~/.pwcli/` home directory.

2. **Configure your AI model providers**:
   Open `~/.pwcli/config.json` and set up your preferred provider and API keys. Refer to [docs/configuration.md](docs/configuration.md) for structure and schema.

---

## Running the Application

### Command Line Interface (CLI/TUI)
Run the interactive console directly in your terminal:
```bash
cargo run
```
Inside the interactive shell, type `/help` to see all available commands.

### Web Workbench
To run the full web application with the backend API and frontend UI:

1. **Start the backend server**:
   ```bash
   cargo run -- serve --open
   ```
   *The `--open` flag will automatically open the browser to the workbench URL.*

2. **(Optional) Run frontend in development mode**:
   If you want to edit the frontend code with Hot Module Replacement:
   ```bash
   cd web
   npm install
   npm run dev
   ```

---

## CLI Reference

`pwcli` can be run in single-command mode from your shell:

| Command | Description |
| :--- | :--- |
| `pwcli init` | Initializes pwcli home directory. |
| `pwcli tools list` | Lists all loaded and available tools. |
| `pwcli tools doctor` | Runs health checks on all registered tools. |
| `pwcli mcp` | Displays or configures active Model Context Protocol servers. |
| `pwcli run "<prompt>"` | Executes a one-off prompt using the active agent loop. |
| `pwcli serve` | Launches the web server. |
| `pwcli audit` | Views and summarizes the execution logs. |
| `pwcli doctor` | Prints diagnostics of the local system. |

---

## License

This project is licensed under the MIT License. See [LICENSE](LICENSE) for details.
