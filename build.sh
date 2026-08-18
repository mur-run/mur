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

# Honor CARGO_TARGET_DIR: cargo writes there, so hardcoding $SCRIPT_DIR/target
# made the script look for a binary cargo never put there — and every later
# `[ -f ... ]` guard then quietly skipped its file.
TARGET_DIR="${CARGO_TARGET_DIR:-$SCRIPT_DIR/target}"
if [ "$RELEASE" = "--release" ]; then
  BINARY="$TARGET_DIR/release/mur"
else
  BINARY="$TARGET_DIR/debug/mur"
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

  # `mur` is not optional. Every other artifact is guarded by `[ -f ]` because a
  # partial workspace build legitimately lacks some of them, but if the main
  # binary is missing then the build did not produce what we are about to claim
  # we installed — and a run that installs nothing must not print a tick.
  if [ ! -f "$BINARY" ]; then
    echo "❌ $BINARY does not exist — nothing to install."
    echo "   (cargo writes to \$CARGO_TARGET_DIR when set; this script looked in $TARGET_DIR)"
    exit 1
  fi

  # Install into a user-writable dir — never through Homebrew's symlink.
  #
  # `/opt/homebrew/bin/mur` is a symlink into the keg, and `cp` follows symlinks,
  # so `sudo cp` here was silently overwriting
  # `/opt/homebrew/Cellar/mur/<version>/bin/mur`. Brew never notices (it does not
  # checksum installed kegs), so `mur --version` kept reporting the released
  # version while running a local build, and the next `brew upgrade`/`reinstall`
  # reverted the dev build with no message. Writing to our own directory instead
  # leaves the package manager's files alone — and needs no sudo at all, which
  # also removes the password prompt that made `--install` unusable from any
  # non-interactive shell.
  INSTALL_DIR="${MUR_INSTALL_DIR:-$HOME/.local/bin}"
  mkdir -p "$INSTALL_DIR"
  echo "📥 Installing to $INSTALL_DIR..."

  # Atomic cp+mv: a running agent holding the old inode would otherwise give
  # ETXTBSY. It keeps its process (and its old binary) until restarted.
  install_bin() {
    src="$1"; name="$2"
    [ -f "$src" ] || return 0
    cp "$src" "$INSTALL_DIR/.$name.new"
    mv -f "$INSTALL_DIR/.$name.new" "$INSTALL_DIR/$name"
    echo "Installed $name -> $INSTALL_DIR/$name"
  }

  # mur-research-gateway is spawned by name off PATH from inside agent sandboxes,
  # so $INSTALL_DIR must stay one of `standard_exec_dirs()` in
  # mur-agent-runtime/src/exec_dirs.rs. mur-agent-runtime is what agent symlinks
  # resolve to, so a stale copy here means new and restarted agents inherit it.
  for b in mur mur-mcp-server mur-research-gateway murmurd mur-agent-runtime; do
    install_bin "$BIN_DIR/$b" "$b"
  done
  ln -sfn "$INSTALL_DIR/mur" "$INSTALL_DIR/murmur"
  echo "Installed murmur -> $INSTALL_DIR/mur (symlink)"

  # An install nothing can reach is not an install. `standard_exec_dirs()` puts
  # /opt/homebrew/bin BEFORE ~/.local/bin, so a copy left there by an older
  # `--install` (or by Homebrew) shadows what we just wrote — for the shell AND
  # for every agent sandbox. Name it rather than let the next upgrade look like
  # it did nothing.
  SHADOWED=false
  for b in mur mur-mcp-server mur-research-gateway murmurd mur-agent-runtime; do
    [ -f "$INSTALL_DIR/$b" ] || continue
    winner="$(command -v "$b" 2>/dev/null || true)"
    if [ -n "$winner" ] && [ "$winner" != "$INSTALL_DIR/$b" ]; then
      echo "⚠ $b: PATH resolves to $winner, NOT the copy just installed"
      SHADOWED=true
    fi
  done
  if $SHADOWED; then
    echo "  Older copies shadow this install. Remove them, e.g.:"
    echo "    sudo rm /opt/homebrew/bin/{mur,murmur,mur-mcp-server,mur-research-gateway,murmurd,mur-agent-runtime}"
    echo "  (a Homebrew-managed 'mur' can be restored with: brew link --overwrite mur)"
    echo "  Then re-check with: command -v mur"
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
  elif [ "$CODESIGN_IDENTITY" = "-" ]; then
    # The default identity IS ad-hoc; the SIGN_FAILED branch above only fires
    # when codesign errored, so without this the common case says nothing.
    echo "⚠ installed with AD-HOC signatures (default). Every rebuild gets a new"
    echo "  identity, so keychain/TCC grants die on each install and agent runtime"
    echo "  attestation treats the binaries as unsigned. For a stable identity:"
    echo "    MUR_CODESIGN_IDENTITY='Developer ID Application: …' ./build.sh --release --install"
  fi

  # Report the version of the copy we actually wrote, not whatever `mur`
  # happens to resolve to — those differ exactly when the shadow warning above
  # fired, which is precisely when a wrong version string would mislead.
  echo "✅ Installed: $("$INSTALL_DIR/mur" --version 2>/dev/null || echo 'done')"
fi
