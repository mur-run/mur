#!/usr/bin/env bash
set -euo pipefail
# Freeze mlx-vlm's OpenAI-compatible server into a single `mlx-server` binary
# with the target-triple suffix Tauri expects, placed in src-tauri/binaries/.
#
# We bundle mlx-vlm (not text-only mlx-lm) because the Hub's media tools need
# vision: `scene_explain` sends `image_url` content, which mlx_lm.server rejects
# with `{"error":"Only 'text' content type is supported."}`. mlx_vlm.server serves
# BOTH text (video_analyze) and vision (scene_explain) on one endpoint, so it is
# the correct single sidecar. mlx-vlm depends on mlx-lm, so text generation is
# unchanged.
#
# Requires: python3.11+, pip, pyinstaller, mlx-vlm (Apple Silicon).

TRIPLE="${1:-aarch64-apple-darwin}"
OUT_DIR="mur-hub-gui/src-tauri/binaries"
mkdir -p "$OUT_DIR"

python3 -m venv .mlxbuild
# shellcheck disable=SC1091
source .mlxbuild/bin/activate
pip install --upgrade pip
pip install mlx-vlm pyinstaller

# Entry script: launch mlx_vlm.server passing through CLI args.
cat > .mlxbuild/entry.py <<'PY'
from mlx_vlm.server import main
if __name__ == "__main__":
    main()
PY

pyinstaller --onefile --name mlx-server .mlxbuild/entry.py \
  --distpath "$OUT_DIR-dist"
mv "$OUT_DIR-dist/mlx-server" "$OUT_DIR/mlx-server-$TRIPLE"
chmod +x "$OUT_DIR/mlx-server-$TRIPLE"
echo "Built $OUT_DIR/mlx-server-$TRIPLE"
