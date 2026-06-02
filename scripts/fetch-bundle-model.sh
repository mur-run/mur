#!/usr/bin/env bash
set -euo pipefail
# Download the default bundled MLX model into the resources dir. NOT committed.
# Model id mirrors DEFAULT_BUNDLED_MODEL_ID in mur-common.

MODEL_REPO="${1:-mlx-community/Qwen3.5-2B-MLX-4bit}"
DEST="mur-hub-gui/src-tauri/resources/models/default"
mkdir -p "$DEST"

VENV_DIR="$(mktemp -d)/hf-venv"
python3 -m venv "$VENV_DIR"
"$VENV_DIR/bin/pip" install --quiet huggingface_hub
"$VENV_DIR/bin/python3" - "$MODEL_REPO" "$DEST" <<'PY'
import sys
from huggingface_hub import snapshot_download
repo, dest = sys.argv[1], sys.argv[2]
snapshot_download(repo_id=repo, local_dir=dest, local_dir_use_symlinks=False)
print(f"Downloaded {repo} -> {dest}")
PY
