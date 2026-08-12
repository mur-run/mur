#!/bin/bash
set -euo pipefail

# Build mur with embedded web dashboard
# Usage: ./build.sh [--release] [--install]

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MUR_WEB_DIR="${MUR_WEB_DIR:-$HOME/Projects/mur-web}"

RELEASE=""
INSTALL=false

for arg in "$@"; do
  case $arg in
    --release) RELEASE="--release" ;;
    --install) INSTALL=true ;;
    *) echo "Unknown arg: $arg"; exit 1 ;;
  esac
done

# Default to release build
if [ -z "$RELEASE" ]; then
  RELEASE="--release"
fi

# Step 1: Build mur-web
echo "📦 Building mur-web..."
if [ ! -d "$MUR_WEB_DIR" ]; then
  echo "❌ mur-web not found at $MUR_WEB_DIR"
  echo "   Set MUR_WEB_DIR to override"
  exit 1
fi

(cd "$MUR_WEB_DIR" && npm run build)
echo "✅ mur-web built"

# Step 2: Build mur-core with embedded dashboard
echo "🔨 Building mur (with embedded dashboard)..."
cd "$SCRIPT_DIR"
MUR_WEB_DIST="$MUR_WEB_DIR/dist" cargo build $RELEASE

echo "✅ Build complete"

if [ "$RELEASE" = "--release" ]; then
  BINARY="$SCRIPT_DIR/target/release/mur"
else
  BINARY="$SCRIPT_DIR/target/debug/mur"
fi

echo "   Binary: $BINARY"
echo "   Size: $(du -h "$BINARY" | cut -f1)"

# Step 3: Install if requested
if $INSTALL; then
  # One-time setup for a stable signing identity (avoids TCC re-prompts on every
  # rebuild): ad-hoc signing (-s -) gives each build a fresh CDHash, so macOS
  # treats it as a new binary and re-prompts for removable-volume access every
  # time. launchd-managed background agents can't show that prompt, so their
  # file operations just hang. To fix: create or pick a stable signing
  # certificate in Keychain Access (a Developer ID certificate, or a
  # self-signed one is fine for local use), then export its name:
  #   export MUR_CODESIGN_IDENTITY="Your Certificate Name"
  # With that set, rebuilds keep a consistent identity and TCC grants survive
  # reinstalls. Leave it unset to keep ad-hoc signing behavior.
  CODESIGN_IDENTITY="${MUR_CODESIGN_IDENTITY:--}"
  SIGN_FAILED=false

  # Sign BEFORE installing, as the invoking user — never under sudo.
  #
  # `sudo codesign -s "Developer ID ..."` cannot succeed: it runs as root, and
  # the identity lives in the *user's* login keychain, so it fails with
  # errSecInternalComponent every single time. This used to run post-copy under
  # sudo with `|| true`, so every install silently shipped an ad-hoc signature —
  # the exact thing MUR_CODESIGN_IDENTITY exists to prevent. A Mach-O signature
  # is embedded in the file, so the `cp` below preserves it.
  sign() {
    [ -f "$1" ] || return 0
    if ! codesign --force -s "$CODESIGN_IDENTITY" "$1"; then
      echo "⚠ codesign FAILED for $1 (identity: $CODESIGN_IDENTITY)"
      SIGN_FAILED=true
    fi
  }

  BIN_DIR="$(dirname "$BINARY")"
  for b in mur mur-mcp-server mur-research-gateway murmurd mur-agent-runtime; do
    sign "$BIN_DIR/$b"
  done

  echo "📥 Installing to /opt/homebrew/bin/mur..."
  sudo cp "$BINARY" /opt/homebrew/bin/mur
  sudo ln -sfn /opt/homebrew/bin/mur /opt/homebrew/bin/murmur
  echo "Installed murmur -> /opt/homebrew/bin/mur (symlink)"

  MCP_BINARY="$SCRIPT_DIR/target/release/mur-mcp-server"
  if [ -f "$MCP_BINARY" ]; then
    sudo cp "$MCP_BINARY" /opt/homebrew/bin/mur-mcp-server
    echo "Installed mur-mcp-server -> /opt/homebrew/bin/mur-mcp-server"
  fi

  # The research gateway (stdio MCP server for tiered web fetch/search).
  # Agent sandboxes spawn it by name off PATH, so it must land next to `mur`.
  GATEWAY_BINARY="$SCRIPT_DIR/target/release/mur-research-gateway"
  if [ -f "$GATEWAY_BINARY" ]; then
    sudo cp "$GATEWAY_BINARY" /opt/homebrew/bin/mur-research-gateway
    echo "Installed mur-research-gateway -> /opt/homebrew/bin/mur-research-gateway"
  fi

  # The background daemon. `mur daemon start` looks for `murmurd` alongside
  # `mur`, so install it too — otherwise the daemon (and the Hub/phone voice
  # path that runs through it) is unavailable on a release install.
  DAEMON_BINARY="$SCRIPT_DIR/target/release/murmurd"
  if [ -f "$DAEMON_BINARY" ]; then
    sudo cp "$DAEMON_BINARY" /opt/homebrew/bin/murmurd
    echo "Installed murmurd -> /opt/homebrew/bin/murmurd"
  fi

  # mur-agent-runtime (already built by the workspace build above) is what new
  # agents symlink to via PATH (resolve_runtime_target). Keep the canonical copy at
  # ~/.local/bin fresh, else newly-created/restarted agents inherit a STALE runtime
  # (e.g. one missing channel/delegate). Atomic cp+mv avoids ETXTBSY for agents
  # running off it; they pick up the new binary on their next restart.
  RUNTIME_BINARY="$(dirname "$BINARY")/mur-agent-runtime"
  LOCAL_BIN="$HOME/.local/bin"; mkdir -p "$LOCAL_BIN"
  if [ -f "$RUNTIME_BINARY" ]; then
    cp "$RUNTIME_BINARY" "$LOCAL_BIN/.mur-agent-runtime.new"
    mv -f "$LOCAL_BIN/.mur-agent-runtime.new" "$LOCAL_BIN/mur-agent-runtime"
    echo "Installed mur-agent-runtime -> $LOCAL_BIN/mur-agent-runtime (canonical; keeps new agents current)"
  fi

  # Nudge: running agents keep their OLD process until restarted, so they're
  # still on the pre-upgrade runtime. Tell the operator (print-only; never auto-restart).
  if command -v mur >/dev/null 2>&1; then
    STALE=$(mur agent runtime-doctor --json 2>/dev/null | grep -c '"stale": *true' || true)
    if [ "${STALE:-0}" -gt 0 ]; then
      echo "⚠ $STALE agent(s) are running a stale runtime — run 'mur agent restart --stale' (--dry-run to list)"
    fi
  fi

  if $SIGN_FAILED; then
    echo "⚠ installed with an AD-HOC signature — every rebuild looks like a new"
    echo "  program to macOS, so headless agents can lose keychain/TCC grants."
    echo "  Check that '$CODESIGN_IDENTITY' is in your login keychain, then re-run."
  fi

  echo "✅ Installed: $(mur --version 2>/dev/null || echo 'done')"
fi
