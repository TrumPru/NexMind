#!/usr/bin/env bash
#
# NexMind Installer
# Installs NexMind AI assistant from source.
#
# Program files  → ~/nexmind/
# User config    → ~/.nexmind/
#
set -euo pipefail

# ── Constants ─────────────────────────────────────────────────
INSTALL_DIR="$HOME/nexmind"
CONFIG_DIR="$HOME/.nexmind"
DATA_DIR="$CONFIG_DIR/data"
SKILLS_DIR="$CONFIG_DIR/skills"
CONFIG_FILE="$CONFIG_DIR/config.toml"
ENV_FILE="$CONFIG_DIR/.env"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Colors ────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ── Result variables ──────────────────────────────────────────
SELECTED_PROVIDER=""
SELECTED_MODEL=""
API_KEY_VAR=""
API_KEY_VALUE=""
TG_BOT_TOKEN=""
TG_ALLOWED_USERS=""

# ── Helpers ───────────────────────────────────────────────────
info()    { echo -e "${BLUE}[INFO]${NC} $*"; }
success() { echo -e "${GREEN}[OK]${NC} $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $*"; }
err()     { echo -e "${RED}[ERROR]${NC} $*"; }

# ── Banner ────────────────────────────────────────────────────
print_banner() {
    echo -e "${CYAN}${BOLD}"
    cat << 'BANNER'

    _   _           __  __ _           _
   | \ | |         |  \/  (_)         | |
   |  \| | _____  _| \  / |_ _ __  __| |
   | . ` |/ _ \ \/ / |\/| | | '_ \/ _` |
   | |\  |  __/>  <| |  | | | | | | (_| |
   |_| \_|\___/_/\_\_|  |_|_|_| |_|\__,_|

         AI Assistant — Installer

BANNER
    echo -e "${NC}"
}

# ── Prerequisites ─────────────────────────────────────────────
check_prerequisites() {
    info "Checking prerequisites..."
    local missing=0

    # Rust / Cargo
    if command -v cargo &>/dev/null; then
        success "cargo $(cargo --version | awk '{print $2}')"
    else
        err "cargo not found. Install Rust: https://rustup.rs"
        echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        missing=1
    fi

    # protoc (required by tonic-build)
    if command -v protoc &>/dev/null; then
        success "protoc $(protoc --version | awk '{print $2}')"
    else
        err "protoc not found. Install protobuf compiler:"
        echo "    Ubuntu/Debian: sudo apt install protobuf-compiler"
        echo "    macOS:         brew install protobuf"
        echo "    Arch:          sudo pacman -S protobuf"
        missing=1
    fi

    # C compiler
    if command -v cc &>/dev/null || command -v gcc &>/dev/null; then
        success "C compiler found"
    else
        err "C compiler not found. Install gcc/build-essential:"
        echo "    Ubuntu/Debian: sudo apt install build-essential"
        missing=1
    fi

    if [ "$missing" -ne 0 ]; then
        echo ""
        err "Please install the missing dependencies and re-run the installer."
        exit 1
    fi

    echo ""
}

# ── AI Model Selection ───────────────────────────────────────
select_ai_model() {
    echo -e "${BOLD}Select your AI model provider:${NC}"
    echo ""
    echo "  1) Anthropic API        (requires API key)"
    echo "     claude-sonnet-4, claude-opus-4, claude-haiku-3.5"
    echo ""
    echo "  2) Claude Code CLI      (free via subscription)"
    echo "     sonnet, opus, haiku"
    echo ""
    echo "  3) OpenAI API           (requires API key)"
    echo "     gpt-4o, gpt-4o-mini"
    echo ""
    echo "  4) Ollama               (local, free)"
    echo "     llama3.2 or any other model"
    echo ""

    local choice
    while true; do
        read -rp "$(echo -e "${CYAN}Provider [1-4]:${NC} ")" choice
        case "$choice" in
            1) select_anthropic; break ;;
            2) select_claude_code; break ;;
            3) select_openai; break ;;
            4) select_ollama; break ;;
            *) warn "Please enter 1, 2, 3, or 4" ;;
        esac
    done
    echo ""
}

select_anthropic() {
    SELECTED_PROVIDER="anthropic"
    API_KEY_VAR="ANTHROPIC_API_KEY"

    echo ""
    echo "  Select model:"
    echo "    1) claude-sonnet-4   (recommended)"
    echo "    2) claude-opus-4"
    echo "    3) claude-haiku-3.5"
    echo ""

    local model_choice
    while true; do
        read -rp "$(echo -e "${CYAN}Model [1-3]:${NC} ")" model_choice
        case "$model_choice" in
            1) SELECTED_MODEL="anthropic/claude-sonnet-4-20250514"; break ;;
            2) SELECTED_MODEL="anthropic/claude-opus-4-20250514"; break ;;
            3) SELECTED_MODEL="anthropic/claude-haiku-3-5-20241022"; break ;;
            *) warn "Please enter 1, 2, or 3" ;;
        esac
    done

    echo ""
    while true; do
        read -rp "$(echo -e "${CYAN}Anthropic API key:${NC} ")" API_KEY_VALUE
        if [[ "$API_KEY_VALUE" == sk-ant-api* ]]; then
            break
        else
            warn "Key should start with 'sk-ant-api'. Try again."
        fi
    done
}

select_claude_code() {
    SELECTED_PROVIDER="claude-code"

    if command -v claude &>/dev/null; then
        success "Claude Code CLI found"
    else
        warn "Claude Code CLI not found in PATH."
        echo "    Install: npm install -g @anthropic-ai/claude-code"
        echo "    Then:    claude login"
    fi

    echo ""
    echo "  Select model:"
    echo "    1) sonnet   (recommended)"
    echo "    2) opus"
    echo "    3) haiku"
    echo ""

    local model_choice
    while true; do
        read -rp "$(echo -e "${CYAN}Model [1-3]:${NC} ")" model_choice
        case "$model_choice" in
            1) SELECTED_MODEL="claude-code/sonnet"; break ;;
            2) SELECTED_MODEL="claude-code/opus"; break ;;
            3) SELECTED_MODEL="claude-code/haiku"; break ;;
            *) warn "Please enter 1, 2, or 3" ;;
        esac
    done
}

select_openai() {
    SELECTED_PROVIDER="openai"
    API_KEY_VAR="OPENAI_API_KEY"

    echo ""
    echo "  Select model:"
    echo "    1) gpt-4o       (recommended)"
    echo "    2) gpt-4o-mini"
    echo ""

    local model_choice
    while true; do
        read -rp "$(echo -e "${CYAN}Model [1-2]:${NC} ")" model_choice
        case "$model_choice" in
            1) SELECTED_MODEL="openai/gpt-4o"; break ;;
            2) SELECTED_MODEL="openai/gpt-4o-mini"; break ;;
            *) warn "Please enter 1 or 2" ;;
        esac
    done

    echo ""
    read -rp "$(echo -e "${CYAN}OpenAI API key:${NC} ")" API_KEY_VALUE
}

select_ollama() {
    SELECTED_PROVIDER="ollama"

    if curl -s --max-time 2 http://localhost:11434/api/tags &>/dev/null; then
        success "Ollama is running on localhost:11434"
    else
        warn "Ollama is not reachable at localhost:11434."
        echo "    Install: https://ollama.com"
        echo "    Start:   ollama serve"
    fi

    echo ""
    read -rp "$(echo -e "${CYAN}Model name [llama3.2]:${NC} ")" local_model
    local_model="${local_model:-llama3.2}"
    SELECTED_MODEL="ollama/${local_model}"
}

# ── Telegram Config ───────────────────────────────────────────
collect_telegram() {
    echo -e "${BOLD}Telegram Bot Configuration${NC}"
    echo ""

    read -rp "$(echo -e "${CYAN}Telegram bot token:${NC} ")" TG_BOT_TOKEN

    if [ -z "$TG_BOT_TOKEN" ]; then
        warn "Skipping Telegram setup (no token provided)"
        return
    fi

    read -rp "$(echo -e "${CYAN}Allowed user IDs (comma-separated, leave empty for all):${NC} ")" TG_ALLOWED_USERS
    echo ""
}

# ── Build ─────────────────────────────────────────────────────
build_project() {
    info "Building NexMind from source (this may take a few minutes)..."
    echo ""
    cd "$SCRIPT_DIR"
    cargo build --release
    echo ""
    success "Build completed"
    echo ""
}

# ── Install Files ─────────────────────────────────────────────
install_files() {
    info "Installing files..."

    # Create directories
    mkdir -p "$INSTALL_DIR/bin"
    mkdir -p "$INSTALL_DIR/skills"
    mkdir -p "$CONFIG_DIR"
    mkdir -p "$DATA_DIR/workspace"
    mkdir -p "$SKILLS_DIR"

    # Copy binaries
    cp "$SCRIPT_DIR/target/release/nexmind" "$INSTALL_DIR/bin/"
    cp "$SCRIPT_DIR/target/release/nexmind-daemon" "$INSTALL_DIR/bin/"

    # Copy built-in skills
    if [ -d "$SCRIPT_DIR/skills/builtin" ]; then
        cp -r "$SCRIPT_DIR/skills/builtin/"* "$INSTALL_DIR/skills/" 2>/dev/null || true
    fi

    success "Binaries installed to $INSTALL_DIR/bin/"
    success "Skills installed to $INSTALL_DIR/skills/"
}

# ── Create config.toml ───────────────────────────────────────
create_config() {
    local tg_enabled="false"
    if [ -n "$TG_BOT_TOKEN" ]; then
        tg_enabled="true"
    fi

    cat > "$CONFIG_FILE" << EOF
# NexMind Configuration
# Generated by install.sh on $(date -Iseconds)

[model]
default = "$SELECTED_MODEL"

[daemon]
address = "127.0.0.1"
port = 19384

[paths]
data_dir = "~/.nexmind/data"
workspace_dir = "~/.nexmind/data/workspace"
builtin_skills_dir = "~/nexmind/skills"
user_skills_dir = "~/.nexmind/skills"

[telegram]
enabled = $tg_enabled
EOF

    success "Config saved to $CONFIG_FILE"
}

# ── Create .env ───────────────────────────────────────────────
create_env() {
    {
        echo "# NexMind Secrets"
        echo "# Generated by install.sh on $(date -Iseconds)"
        echo "# DO NOT commit this file to version control"
        echo ""

        # API key
        if [ -n "$API_KEY_VAR" ] && [ -n "$API_KEY_VALUE" ]; then
            echo "$API_KEY_VAR=$API_KEY_VALUE"
        else
            echo "# ANTHROPIC_API_KEY="
            echo "# OPENAI_API_KEY="
        fi

        echo ""

        # Telegram
        if [ -n "$TG_BOT_TOKEN" ]; then
            echo "TELEGRAM_BOT_TOKEN=$TG_BOT_TOKEN"
            if [ -n "$TG_ALLOWED_USERS" ]; then
                echo "TELEGRAM_ALLOWED_USERS=$TG_ALLOWED_USERS"
            fi
        else
            echo "# TELEGRAM_BOT_TOKEN="
            echo "# TELEGRAM_ALLOWED_USERS="
        fi
    } > "$ENV_FILE"

    chmod 600 "$ENV_FILE"
    success "Secrets saved to $ENV_FILE (chmod 600)"
}

# ── Create Launcher ──────────────────────────────────────────
create_launcher() {
    cat > "$INSTALL_DIR/bin/nexmind-start" << 'LAUNCHER'
#!/usr/bin/env bash
# NexMind Daemon Launcher
set -euo pipefail

ENV_FILE="$HOME/.nexmind/.env"
if [ -f "$ENV_FILE" ]; then
    set -a
    source "$ENV_FILE"
    set +a
fi

exec "$HOME/nexmind/bin/nexmind-daemon" \
    --data-dir "$HOME/.nexmind/data" \
    --workspace-dir "$HOME/.nexmind/data/workspace"
LAUNCHER

    chmod +x "$INSTALL_DIR/bin/nexmind-start"
    success "Launcher created: $INSTALL_DIR/bin/nexmind-start"

    # Systemd user service (Linux only)
    if [ -d "/run/systemd/system" ] 2>/dev/null; then
        local service_dir="$HOME/.config/systemd/user"
        mkdir -p "$service_dir"
        cat > "$service_dir/nexmind.service" << EOF
[Unit]
Description=NexMind AI Assistant Daemon
After=network.target

[Service]
Type=simple
EnvironmentFile=%h/.nexmind/.env
ExecStart=%h/nexmind/bin/nexmind-daemon \\
    --data-dir %h/.nexmind/data \\
    --workspace-dir %h/.nexmind/data/workspace
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
        success "Systemd service: $service_dir/nexmind.service"
    fi
}

# ── Setup PATH ────────────────────────────────────────────────
setup_path() {
    local bin_dir="$INSTALL_DIR/bin"

    # Check if already in PATH
    if echo "$PATH" | tr ':' '\n' | grep -qF "$bin_dir"; then
        success "$bin_dir already in PATH"
        return
    fi

    local export_line="export PATH=\"\$HOME/nexmind/bin:\$PATH\""

    echo ""
    read -rp "$(echo -e "${CYAN}Add $bin_dir to PATH? [Y/n]:${NC} ")" add_path
    add_path="${add_path:-Y}"

    if [[ "$add_path" =~ ^[Yy] ]]; then
        local shell_rc=""
        case "$(basename "${SHELL:-bash}")" in
            zsh)  shell_rc="$HOME/.zshrc" ;;
            bash) shell_rc="$HOME/.bashrc" ;;
            *)    shell_rc="$HOME/.profile" ;;
        esac

        echo "" >> "$shell_rc"
        echo "# NexMind" >> "$shell_rc"
        echo "$export_line" >> "$shell_rc"

        success "Added to $shell_rc"
        warn "Run 'source $shell_rc' or restart your terminal"
    else
        info "Skipped. Add manually: $export_line"
    fi
}

# ── Summary ───────────────────────────────────────────────────
print_summary() {
    echo ""
    echo -e "${GREEN}${BOLD}NexMind installed successfully!${NC}"
    echo ""
    echo -e "  ${BOLD}Binaries:${NC}     ~/nexmind/bin/"
    echo -e "  ${BOLD}Config:${NC}       ~/.nexmind/config.toml"
    echo -e "  ${BOLD}Secrets:${NC}      ~/.nexmind/.env"
    echo -e "  ${BOLD}Data:${NC}         ~/.nexmind/data/"
    echo -e "  ${BOLD}User skills:${NC}  ~/.nexmind/skills/"
    echo -e "  ${BOLD}Model:${NC}        $SELECTED_MODEL"
    if [ -n "$TG_BOT_TOKEN" ]; then
        echo -e "  ${BOLD}Telegram:${NC}     configured"
    fi
    echo ""
    echo -e "  ${BOLD}Start daemon:${NC}  nexmind-start"
    echo -e "  ${BOLD}Use CLI:${NC}       nexmind chat \"Hello\""
    if [ -d "/run/systemd/system" ] 2>/dev/null; then
        echo -e "  ${BOLD}Systemd:${NC}       systemctl --user enable --now nexmind"
    fi
    echo ""
}

# ── Main ──────────────────────────────────────────────────────
main() {
    print_banner
    check_prerequisites
    select_ai_model
    collect_telegram
    build_project
    install_files
    create_config
    create_env
    create_launcher
    setup_path
    print_summary
}

main "$@"
