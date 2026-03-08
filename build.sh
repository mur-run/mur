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
  echo "📥 Installing to /usr/local/bin/mur..."
  cp "$BINARY" /usr/local/bin/mur
  echo "✅ Installed: $(mur --version 2>/dev/null || echo 'done')"
fi
