#!/bin/sh
# install.sh — one-liner installer for mur CLI.
# Usage: curl -fsSL https://mur.run/install.sh | sh
#        curl -fsSL https://mur.run/install.sh | sh -s -- --version 2.16.1
# Env:   MUR_INSTALL_DIR (default: $HOME/.local/bin)

set -eu

GH_OWNER="mur-run"
GH_REPO="mur"
SILENT=0
VERSION=""

log() { [ "$SILENT" = "1" ] || printf '%s\n' "$*"; }
err() { printf 'error: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }

# --- Parse args ---
while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:-}"; shift 2 ;;
        --version=*) VERSION="${1#--version=}"; shift ;;
        -s|--silent) SILENT=1; shift ;;
        -h|--help)
            printf 'Usage: install.sh [--version X.Y.Z] [-s]\n'
            exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

# --- Detect OS / arch ---
uname_s=$(uname -s 2>/dev/null || echo unknown)
uname_m=$(uname -m 2>/dev/null || echo unknown)

case "$uname_s" in
    Darwin)
        case "$uname_m" in
            arm64|aarch64) ASSET="mur-aarch64-apple-darwin.tar.gz" ;;
            *) die "macOS on $uname_m is not supported. Install from source: cargo install mur" ;;
        esac ;;
    Linux)
        case "$uname_m" in
            x86_64|amd64) ASSET="mur-x86_64-unknown-linux-gnu.tar.gz" ;;
            aarch64|arm64) die "No prebuilt Linux arm64 binary yet. Install from source: cargo install mur" ;;
            *) die "Linux on $uname_m is not supported." ;;
        esac ;;
    MINGW*|MSYS*|CYGWIN*)
        die "On Windows, run instead: irm https://mur.run/install.ps1 | iex" ;;
    *) die "Unsupported OS: $uname_s" ;;
esac

# --- Resolve version ---
if [ -z "$VERSION" ]; then
    log "Resolving latest mur release..."
    API_URL="https://api.github.com/repos/$GH_OWNER/$GH_REPO/releases/latest"
    if command -v curl >/dev/null 2>&1; then
        LATEST_JSON=$(curl -fsSL "$API_URL" 2>/dev/null) || die "Could not reach GitHub. Check your connection."
    elif command -v wget >/dev/null 2>&1; then
        LATEST_JSON=$(wget -qO- "$API_URL") || die "Could not reach GitHub. Check your connection."
    else
        die "Neither curl nor wget is installed."
    fi
    TAG=$(printf '%s' "$LATEST_JSON" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)
    [ -n "$TAG" ] || die "Could not parse latest tag from GitHub API."
    VERSION="${TAG#v}"
fi

REL_URL="https://github.com/$GH_OWNER/$GH_REPO/releases/download/v${VERSION}"
ASSET_URL="$REL_URL/$ASSET"
CHECKSUMS_URL="$REL_URL/checksums.txt"

# --- Pick install dir ---
DEFAULT_DIR="${HOME:-/root}/.local/bin"
INSTALL_DIR="${MUR_INSTALL_DIR:-$DEFAULT_DIR}"
mkdir -p "$INSTALL_DIR" 2>/dev/null || true
if ! [ -w "$INSTALL_DIR" ]; then
    if [ -w /usr/local/bin ]; then
        INSTALL_DIR=/usr/local/bin
    else
        die "$INSTALL_DIR is not writable. Re-run with: MUR_INSTALL_DIR=/usr/local/bin sudo sh install.sh"
    fi
fi

# --- Download + verify ---
TMP=$(mktemp -d 2>/dev/null || mktemp -d -t mur-install)
trap 'rm -rf "$TMP"' EXIT

log "Downloading $ASSET (v$VERSION)..."
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$ASSET_URL"     -o "$TMP/$ASSET"         || die "Download failed."
    curl -fsSL "$CHECKSUMS_URL" -o "$TMP/checksums.txt"  || die "Download failed."
else
    wget -q "$ASSET_URL"     -O "$TMP/$ASSET"        || die "Download failed."
    wget -q "$CHECKSUMS_URL" -O "$TMP/checksums.txt" || die "Download failed."
fi

# The release job hashes `./<file>`, so the listed names carry a `./` prefix;
# sha256sum's binary mode would add a `*` instead. Normalise the name and match
# it whole, rather than substring-matching the tail of the line.
EXPECTED=$(awk -v f="$ASSET" '{ n=$2; sub(/^[*]/,"",n); sub(/^\.\//,"",n); if (n==f) print $1 }' \
    "$TMP/checksums.txt" | head -n1)
[ -n "$EXPECTED" ] || die "No checksum entry for $ASSET in checksums.txt."

if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL=$(sha256sum "$TMP/$ASSET" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
    ACTUAL=$(shasum -a 256 "$TMP/$ASSET" | awk '{print $1}')
else
    die "Need sha256sum or shasum to verify download."
fi

[ "$EXPECTED" = "$ACTUAL" ] || die "Checksum verification FAILED. Aborting."

# --- Extract + install ---
tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -f "$TMP/mur" ] || die "Archive did not contain a mur binary."
# Keep this list in sync with the Homebrew formula's `install` block.
# Optional entries are skipped when an older release did not ship them.
for b in mur mur-mcp-server murmurd mur-agent-runtime; do
    [ -f "$TMP/$b" ] || continue
    chmod +x "$TMP/$b"
    mv "$TMP/$b" "$INSTALL_DIR/$b"
done
# `murmur` is argv[0] shorthand for `mur agent cli` (BusyBox convention).
ln -sfn mur "$INSTALL_DIR/murmur"

# --- PATH check ---
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        log ""
        log "Installed to $INSTALL_DIR but it is not on your PATH."
        log "Add this to your shell profile:"
        log "    export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

log ""
log "mur v$VERSION installed."
log "Run 'mur init' to get started."
