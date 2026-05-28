#!/usr/bin/env bash
# klaar installer
# Builds the binary, installs it to /usr/local/bin, and patches your AI
# environment configs (Google Antigravity + Claude Code).
set -euo pipefail

BINARY="klaar"
INSTALL_DIR="/usr/local/bin"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOME_DIR="${HOME:-$(eval echo ~)}"

# ── Pretty print helpers ───────────────────────────────────────────────────────
ok()   { echo "  ✅  $*"; }
info() { echo "  ℹ️   $*"; }
warn() { echo "  ⚠️   $*"; }
step() { echo ""; echo "▶ $*"; }

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  klaar — AI coding agent optimizer installer"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# ── 1. Check Rust is available ─────────────────────────────────────────────────
if ! command -v cargo &>/dev/null; then
  echo ""
  warn "cargo not found. Install Rust first: https://rustup.rs"
  exit 1
fi

# ── 2. Build ───────────────────────────────────────────────────────────────────
step "Building klaar (release mode)..."
cd "$PROJECT_ROOT"
cargo build --release
ok "Build complete → target/release/$BINARY"

# ── 3. Install binary ──────────────────────────────────────────────────────────
step "Installing to $INSTALL_DIR/$BINARY..."
if [ -w "$INSTALL_DIR" ]; then
  cp "$PROJECT_ROOT/target/release/$BINARY" "$INSTALL_DIR/$BINARY"
else
  warn "$INSTALL_DIR is not writable by current user."
  info "You can manually copy the binary to a directory in your PATH if you prefer:"
  info "  cp \"$PROJECT_ROOT/target/release/$BINARY\" ~/bin/$BINARY"
  info "  (or ~/.cargo/bin/$BINARY)"
  info ""
  info "Otherwise, the installer will attempt to use sudo to install it to $INSTALL_DIR:"
  sudo cp "$PROJECT_ROOT/target/release/$BINARY" "$INSTALL_DIR/$BINARY"
fi
ok "Installed: $(which klaar || echo "$INSTALL_DIR/$BINARY")"

BINARY_PATH="$INSTALL_DIR/$BINARY"

# ── 4. Patch AI configs ────────────────────────────────────────────────────────
step "Detecting AI environments..."
echo ""

# Patch a JSON file: add/update mcpServers.<name> entry.
# Uses python3 (always available on macOS) so no jq dependency.
patch_mcp_json() {
  local config_file="$1"
  local server_name="$2"
  local binary="$3"

  python3 - "$config_file" "$server_name" "$binary" <<'PYEOF'
import json, sys, os

config_path, server_name, binary_path = sys.argv[1], sys.argv[2], sys.argv[3]

# Read (start with empty object if file is empty)
with open(config_path) as f:
    content = f.read().strip()
cfg = json.loads(content) if content else {}

if "mcpServers" not in cfg:
    cfg["mcpServers"] = {}

cfg["mcpServers"][server_name] = {
    "command": binary_path,
    "args": ["serve"]
}

with open(config_path, "w") as f:
    json.dump(cfg, f, indent=2)
    f.write("\n")
PYEOF
}

try_patch() {
  local label="$1"
  local config_file="$2"

  if [ ! -f "$config_file" ]; then
    return 1
  fi

  local err_file
  err_file=$(mktemp)

  if patch_mcp_json "$config_file" "klaar" "$BINARY_PATH" 2>"$err_file"; then
    ok "$label → patched $config_file"
    rm -f "$err_file"
    return 0
  else
    warn "$label → failed to patch $config_file"
    if [ -s "$err_file" ]; then
      warn "Python patching error details:"
      cat "$err_file" | sed 's/^/    /'
    fi
    rm -f "$err_file"
    return 1
  fi
}

# Google Antigravity (checks the real location first, then fallback)
AG_DONE=false
for ag_path in \
    "$HOME_DIR/.gemini/antigravity/mcp_config.json" \
    "$HOME_DIR/.gemini/settings.json" \
    "$HOME_DIR/.config/antigravity/settings.json"; do
  if try_patch "Google Antigravity" "$ag_path"; then
    AG_DONE=true
    break
  fi
done
if [ "$AG_DONE" = "false" ]; then
  info "No existing Google Antigravity configuration found. Creating fresh default config..."
  fresh_ag_path="$HOME_DIR/.gemini/antigravity/mcp_config.json"
  if mkdir -p "$(dirname "$fresh_ag_path")" && echo "{}" > "$fresh_ag_path"; then
    if try_patch "Google Antigravity" "$fresh_ag_path"; then
      AG_DONE=true
    fi
  else
    warn "Failed to create default Google Antigravity config path at $fresh_ag_path"
  fi
fi

# Claude Code
CLAUDE_DONE=false
for claude_path in \
    "$HOME_DIR/.config/claude/claude_desktop_config.json" \
    "$HOME_DIR/Library/Application Support/Claude/claude_desktop_config.json"; do
  if try_patch "Claude Code" "$claude_path"; then
    CLAUDE_DONE=true
    break
  fi
done
if [ "$CLAUDE_DONE" = "false" ]; then
  info "No existing Claude Code configuration found. Creating fresh default config..."
  fresh_claude_path="$HOME_DIR/.config/claude/claude_desktop_config.json"
  if mkdir -p "$(dirname "$fresh_claude_path")" && echo "{}" > "$fresh_claude_path"; then
    if try_patch "Claude Code" "$fresh_claude_path"; then
      CLAUDE_DONE=true
    fi
  else
    warn "Failed to create default Claude Code config path at $fresh_claude_path"
  fi
fi

# ── 5. Summary ─────────────────────────────────────────────────────────────────
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  Done!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Next steps:"
echo ""
echo "  1. Restart your AI environment (Antigravity / Claude Code)"
echo "     to pick up the new MCP server."
echo ""
echo "  2. Init a project:"
echo "     klaar install --project /path/to/your-project"
echo "     → Creates .klaar/targets.toml with pre-push check templates"
echo ""
echo "  3. Test it:"
echo "     klaar check --target production --path /path/to/your-project"
echo ""
echo "  4. Wire as a git hook (optional):"
echo "     echo '#!/bin/bash' > .git/hooks/pre-push"
echo "     echo 'klaar check --target production --path \"\$(pwd)\"' >> .git/hooks/pre-push"
echo "     chmod +x .git/hooks/pre-push"
echo ""
echo "MCP tools your agents now have:"
echo "  • surgical_read   — tree-sitter skeleton or full file read"
echo "  • fast_apply      — surgical line-range patch"
echo "  • store_memory    — persist decisions to embedded SurrealDB"
echo "  • recall_memory   — BM25 full-text search of stored memories"
echo "  • pre_push_check  — run .klaar/targets.toml checks before push"
echo ""
