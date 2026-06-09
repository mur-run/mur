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

# Pin mlx-vlm for reproducible release builds. The pinned version MUST support the
# bundled model's architecture (the `model_type` in resources/models/default/
# config.json) — a too-old mlx-vlm raises `ValueError: Model type <t> not supported`
# at load. Bump this alongside the bundled model; override for local experiments.
MLX_VLM_VERSION="${MLX_VLM_VERSION:-0.6.2}"

python3 -m venv .mlxbuild
# shellcheck disable=SC1091
source .mlxbuild/bin/activate
pip install --upgrade pip
pip install "mlx-vlm==${MLX_VLM_VERSION}" pyinstaller

# Entry script: launch mlx_vlm.server passing through CLI args.
cat > .mlxbuild/entry.py <<'PY'
from mlx_vlm.server import main
if __name__ == "__main__":
    main()
PY

# mlx-vlm selects the model implementation at runtime via
# `importlib.import_module(f"mlx_vlm.models.{model_type}")`, and transformers loads
# processor/tokenizer classes (e.g. the bundled Qwen image processor) dynamically by
# name. PyInstaller's static analysis cannot see these, so without --collect-all the
# frozen binary builds clean but dies at model load with ModuleNotFoundError. Collect
# every submodule + data file (and metadata, which transformers reads at import) for
# the dynamic-import packages. --copy-metadata is required because transformers does
# importlib.metadata.version() checks on itself and its deps at import time.
pyinstaller --onefile --name mlx-server .mlxbuild/entry.py \
  --collect-all mlx_vlm \
  --collect-all mlx_lm \
  --collect-all transformers \
  --copy-metadata transformers \
  --copy-metadata mlx-vlm \
  --copy-metadata mlx-lm \
  --distpath "$OUT_DIR-dist"

# Smoke-check that the frozen binary at least imports and runs (catches gross
# PyInstaller breakage). A full model-load smoke needs the bundled model and is
# done in the E2E harness, not here.
"$OUT_DIR-dist/mlx-server" --help >/dev/null 2>&1 \
  || echo "WARN: frozen mlx-server --help failed; check PyInstaller collection" >&2
mv "$OUT_DIR-dist/mlx-server" "$OUT_DIR/mlx-server-$TRIPLE"
chmod +x "$OUT_DIR/mlx-server-$TRIPLE"
echo "Built $OUT_DIR/mlx-server-$TRIPLE"
