#!/usr/bin/env bash
set -euo pipefail
# Freeze mlx-lm's OpenAI-compatible server into a single `mlx-server` binary
# with the target-triple suffix Tauri expects, placed in src-tauri/binaries/.
#
# Requires: python3.11+, pip, pyinstaller, mlx-lm (Apple Silicon).

TRIPLE="${1:-aarch64-apple-darwin}"
OUT_DIR="mur-hub-gui/src-tauri/binaries"
mkdir -p "$OUT_DIR"

python3 -m venv .mlxbuild
# shellcheck disable=SC1091
source .mlxbuild/bin/activate
pip install --upgrade pip
pip install mlx-lm pyinstaller

# Entry script: launch mlx_lm.server passing through CLI args.
cat > .mlxbuild/entry.py <<'PY'
from mlx_lm.server import main
if __name__ == "__main__":
    main()
PY

pyinstaller --onefile --name mlx-server .mlxbuild/entry.py \
  --distpath "$OUT_DIR-dist"
mv "$OUT_DIR-dist/mlx-server" "$OUT_DIR/mlx-server-$TRIPLE"
chmod +x "$OUT_DIR/mlx-server-$TRIPLE"
echo "Built $OUT_DIR/mlx-server-$TRIPLE"
