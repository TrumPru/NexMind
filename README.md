<p align="center">
  <img src="images/logo.png" alt="NexMind" width="180">
</p>

# NexMind

**AI Agent Platform** — run AI agents locally on your computer or server. Agents perform tasks through your preferred messenger, controlling your browser, terminal, files, and email.

## Features

- **Terminal** — execute shell commands on your machine
- **Browser** — navigation, clicks, text input, screenshots, data extraction
- **File System** — read, write, and list files
- **Email** — send, receive, and search emails (IMAP/SMTP)
- **HTTP Requests** — access external APIs
- **Memory** — session and semantic (long-term) memory
- **Multi-Agent** — orchestrate agent teams (sequential/parallel)
- **Scheduler** — run agents on a schedule (cron)
- **Skills** — built-in and extensible skills (weather, web search, translator, math, QR codes, etc.)
- **Cost Tracking** — monitor tokens and budget per agent
- **Approvals** — approval workflows for sensitive actions with risk levels
- **Audit & Security** — logging, HMAC validation

## Architecture

```
apps/
├── daemon/          # Main daemon (gRPC + HTTP dashboard)
└── cli/             # Command-line interface

core/
├── agent-engine/    # Agent definition, registry, and runtime
├── model-router/    # Multi-provider LLM routing
├── tool-runtime/    # Tool execution engine (15+ built-in tools)
├── memory/          # Session + semantic memory
├── scheduler/       # Task scheduler (cron)
├── workflow-engine/ # Multi-step workflow orchestration
├── skill-registry/  # Skill loading and management
├── connector/       # Base connector traits
├── event-bus/       # Event system
├── storage/         # SQLite abstraction
└── security/        # Audit, HMAC

connectors/
├── telegram/        # Telegram bot
└── openclaw/        # OpenClaw gateway

skills/builtin/      # Built-in skills (8)
dashboard/           # Web UI (SPA)
proto/               # Protobuf gRPC API definitions
```

## Tech Stack

- **Rust** + **Tokio** (async runtime)
- **gRPC** (tonic/prost) — primary API
- **Axum** — HTTP server for the dashboard
- **SQLite** (rusqlite + r2d2) — data storage
- **Teloxide** — Telegram integration

### Supported LLM Providers

| Provider | Connection |
|----------|------------|
| Anthropic | API key (`ANTHROPIC_API_KEY`) |
| Claude Code | Subscription |
| OpenAI | API key (`OPENAI_API_KEY`) |
| Ollama | Local inference |

Automatic fallback: if the primary provider is unavailable, the system switches to the next one by priority.

## Installation & Usage

### Requirements

- Rust 1.75+
- SQLite 3
- Chrome/Chromium (optional, for browser automation)
- Ollama (optional, for local inference)

### Build

```bash
cargo build --release
```

### Run the Daemon

```bash
cargo run --bin nexmind-daemon -- \
  --socket-path 127.0.0.1:19384 \
  --data-dir ./data \
  --workspace-dir ./data/workspace
```

### CLI

```bash
nexmind health            # Check daemon status
nexmind chat              # Interactive chat (REPL)
nexmind agent list        # List agents
nexmind agent create      # Create an agent
nexmind schedule list     # List scheduled tasks
nexmind team list         # List agent teams
nexmind cost summary      # Cost statistics
nexmind approve           # Manage approvals
```

### Endpoints

| Service | Address |
|---------|---------|
| gRPC API | `127.0.0.1:19384` |
| HTTP Dashboard | `http://127.0.0.1:19385/?token=<token>` |

## Environment Variables

| Variable | Description |
|----------|-------------|
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `OPENAI_API_KEY` | OpenAI API key |
| `TELEGRAM_BOT_TOKEN` | Telegram bot token |
| `OPENCLAW_GATEWAY_URL` | OpenClaw gateway URL |
| `SMTP_HOST`, `SMTP_PORT`, `SMTP_USER`, `SMTP_PASSWORD` | SMTP settings |
| `IMAP_HOST`, `IMAP_PORT`, `IMAP_USER`, `IMAP_PASSWORD` | IMAP settings |
| `RUST_LOG` | Log level (default: `info`) |

## License

MIT
