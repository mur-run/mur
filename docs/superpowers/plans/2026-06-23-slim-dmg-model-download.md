# Slim DMG + First-Run Model Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop baking the 1.6GB MLX model into the Hub `.dmg`; on first run, auto-detect an existing local runtime, else show a mandatory picker that either downloads the model (cached outside the app) or connects an LLM.

**Architecture:** A new `mur-core` HuggingFace folder-downloader writes the model to a writable `~/.mur/models/<name>/` dir (survives updates). The Hub's mlx sidecar resolves the model from there first, the bundled-resource glob is removed, and `lib.rs` emits a `need-model` event when nothing is configured so the frontend opens the existing Model Library as a mandatory picker with one added "download" tile.

**Tech Stack:** Rust (edition 2024), Tauri 2 (mur-hub-gui, workspace-excluded), reqwest (async, `stream` feature already on), serde_json, mur-common shared paths, mur-web frontend (separate repo `~/Projects/mur-web`).

## Global Constraints

- Rust edition 2024 — `let` chains stable.
- **No hardcoded values** — model repo id, local dir name, HF base URL, completion-marker name are named constants in `mur-common`.
- Brand is uppercase **MUR** in any user-facing string; internal `name`/dirs stay lowercase.
- Model repo: `mlx-community/Qwen3.5-2B-MLX-4bit`. Local dir: `Qwen3.5-2B-MLX-4bit`. Cached at `~/.mur/models/Qwen3.5-2B-MLX-4bit/`, completion sentinel file `.complete`.
- `mur-core` tests/builds need `ORT_STRATEGY=download` and run under **nextest** (plain `cargo test --workspace` is flaky here).
- `mur-hub-gui/src-tauri` is **workspace-excluded**: build/test/fmt it via its own manifest (`--manifest-path mur-hub-gui/src-tauri/Cargo.toml`), separately from the workspace.
- `mur-common` already does small std::fs I/O in `local_llm.rs`; pure path helpers there are consistent — keep network/heavy I/O out of `mur-common`.

---

### Task 1: Shared model-path constants & helpers (mur-common)

**Files:**
- Modify: `mur-common/src/local_llm.rs`

**Interfaces:**
- Produces:
  - `pub const DEFAULT_LOCAL_MODEL_REPO: &str = "mlx-community/Qwen3.5-2B-MLX-4bit";`
  - `pub const DEFAULT_LOCAL_MODEL_DIR: &str = "Qwen3.5-2B-MLX-4bit";`
  - `pub fn local_model_dir(mur_home: &Path, model_dir: &str) -> PathBuf`
  - `pub fn model_complete_marker(model_dir: &Path) -> PathBuf`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `mur-common/src/local_llm.rs`:

```rust
    #[test]
    fn model_dir_and_marker_paths() {
        let home = Path::new("/tmp/murhome");
        let dir = local_model_dir(home, DEFAULT_LOCAL_MODEL_DIR);
        assert_eq!(dir, home.join("models").join("Qwen3.5-2B-MLX-4bit"));
        assert_eq!(model_complete_marker(&dir), dir.join(".complete"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common model_dir_and_marker_paths`
Expected: FAIL — `cannot find function local_model_dir` / `DEFAULT_LOCAL_MODEL_DIR`.

- [ ] **Step 3: Write minimal implementation**

Add near the top of `mur-common/src/local_llm.rs` (after the existing `use`):

```rust
/// HuggingFace repo holding the default bundled-free local model.
pub const DEFAULT_LOCAL_MODEL_REPO: &str = "mlx-community/Qwen3.5-2B-MLX-4bit";
/// Directory name (under `<mur_home>/models/`) for the default local model.
pub const DEFAULT_LOCAL_MODEL_DIR: &str = "Qwen3.5-2B-MLX-4bit";

/// Writable directory where a downloaded local model lives. Outside the app
/// bundle so it survives reinstalls and autoupdates (the download cache).
pub fn local_model_dir(mur_home: &Path, model_dir: &str) -> PathBuf {
    mur_home.join("models").join(model_dir)
}

/// Sentinel file written only after a model download fully completes. Its
/// presence means the directory is a usable, complete model.
pub fn model_complete_marker(model_dir: &Path) -> PathBuf {
    model_dir.join(".complete")
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common model_dir_and_marker_paths`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/local_llm.rs
git commit -m "feat(common): shared local-model dir + completion-marker helpers"
```

---

### Task 2: HuggingFace folder downloader (mur-core)

**Files:**
- Create: `mur-core/src/model_download.rs`
- Modify: `mur-core/src/lib.rs` (register module)

**Interfaces:**
- Consumes: `mur_common::local_llm::model_complete_marker` (Task 1).
- Produces:
  - `pub struct HfFile { pub name: String, pub size: u64 }`
  - `pub fn parse_hf_files(meta: &serde_json::Value) -> Vec<HfFile>`
  - `pub async fn download_hf_model(repo: &str, dest: &std::path::Path, on_progress: impl Fn(u64, u64)) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/model_download.rs` with ONLY the tests first (so it fails to compile against missing items):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_siblings_with_and_without_size() {
        let meta = serde_json::json!({
            "siblings": [
                {"rfilename": "model.safetensors", "size": 1000u64},
                {"rfilename": "config.json"},
            ]
        });
        let files = parse_hf_files(&meta);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0], HfFile { name: "model.safetensors".into(), size: 1000 });
        assert_eq!(files[1], HfFile { name: "config.json".into(), size: 0 });
    }

    #[test]
    fn parses_missing_siblings_as_empty() {
        assert!(parse_hf_files(&serde_json::json!({})).is_empty());
    }

    #[tokio::test]
    async fn complete_marker_short_circuits_without_network() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path();
        std::fs::write(mur_common::local_llm::model_complete_marker(dest), b"ok").unwrap();
        // Bogus repo: must NOT be contacted because the marker exists.
        download_hf_model("does/not-exist", dest, |_, _| {}).await.unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core model_download`
Expected: FAIL — module not declared / `parse_hf_files` not found.

- [ ] **Step 3: Write minimal implementation**

Prepend the implementation above the `#[cfg(test)]` block in `mur-core/src/model_download.rs`:

```rust
//! Download a HuggingFace model repo (a folder of files) into a local dir,
//! reporting aggregate byte progress. Reuses the temp-file + atomic-rename
//! idiom and writes a `.complete` sentinel only on full success, so a partial
//! download never looks cached. Used by the Hub first-run flow (and reusable
//! by the CLI later).

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;

/// HuggingFace site root. Named (not inlined) per the no-hardcoded-values rule.
const HF_BASE: &str = "https://huggingface.co";
/// User-Agent sent with HF requests.
const HF_USER_AGENT: &str = "mur-hub";

/// One file in a HF repo listing.
#[derive(Debug, Clone, PartialEq)]
pub struct HfFile {
    pub name: String,
    pub size: u64,
}

/// Parse the `siblings[]` of a HF `/api/models/<repo>?blobs=true` response into
/// a flat file list. `size` defaults to 0 when the API omits it (progress then
/// reports an indeterminate total).
pub fn parse_hf_files(meta: &serde_json::Value) -> Vec<HfFile> {
    meta.get("siblings")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s.get("rfilename")?.as_str()?.to_string();
                    let size = s.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                    Some(HfFile { name, size })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Download every file of `repo` into `dest`, calling `on_progress(done, total)`
/// (bytes) as it streams. Returns immediately if `dest/.complete` already exists.
pub async fn download_hf_model(
    repo: &str,
    dest: &Path,
    on_progress: impl Fn(u64, u64),
) -> Result<()> {
    let marker = mur_common::local_llm::model_complete_marker(dest);
    if marker.is_file() {
        return Ok(()); // already downloaded — the cache from the design's Q4.
    }
    std::fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;

    let client = reqwest::Client::builder()
        .user_agent(HF_USER_AGENT)
        .build()
        .context("build http client")?;

    let api = format!("{HF_BASE}/api/models/{repo}?blobs=true");
    let meta: serde_json::Value = client
        .get(&api)
        .send()
        .await
        .with_context(|| format!("list {repo}"))?
        .error_for_status()
        .with_context(|| format!("list {repo}"))?
        .json()
        .await
        .context("parse HF model listing")?;

    let files = parse_hf_files(&meta);
    if files.is_empty() {
        bail!("no files listed for HF repo '{repo}'");
    }
    let total: u64 = files.iter().map(|f| f.size).sum();
    let mut done: u64 = 0;

    for f in &files {
        let url = format!("{HF_BASE}/{repo}/resolve/main/{}", f.name);
        let final_path = dest.join(&f.name);
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Temp file lives in `dest` (same filesystem → atomic rename). Sanitize
        // any '/' so a nested rfilename can't escape the temp name.
        let tmp = dest.join(format!("{}.part", f.name.replace('/', "__")));

        let mut resp = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("download {}", f.name))?
            .error_for_status()
            .with_context(|| format!("download {}", f.name))?;

        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        while let Some(chunk) = resp.chunk().await.with_context(|| format!("stream {}", f.name))? {
            file.write_all(&chunk)?;
            done += chunk.len() as u64;
            on_progress(done, total);
        }
        file.flush()?;
        drop(file);
        std::fs::rename(&tmp, &final_path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), final_path.display()))?;
    }

    // Marker last: a partial download is never mistaken for complete.
    std::fs::write(&marker, b"ok").with_context(|| format!("write {}", marker.display()))?;
    Ok(())
}
```

Register the module — add to `mur-core/src/lib.rs` in alphabetical position (after `pub mod model_discovery;` / before `pub mod model_prices;`):

```rust
pub mod model_download;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core model_download`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/model_download.rs mur-core/src/lib.rs
git commit -m "feat(core): HuggingFace folder downloader with completion-marker cache"
```

---

### Task 3: mlx sidecar resolves the writable model dir first

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mlx_sidecar.rs`

**Interfaces:**
- Consumes: `mur_common::local_llm::{local_model_dir, model_complete_marker, DEFAULT_LOCAL_MODEL_DIR}` (Task 1), `crate::mur_home_path()`.
- Produces: `fn downloaded_model_dir(mur_home: &Path) -> Option<PathBuf>` (pure, testable); `model_available`/`start` resolve via it first.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `mur-hub-gui/src-tauri/src/mlx_sidecar.rs`:

```rust
    #[test]
    fn downloaded_model_dir_requires_complete_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // No marker yet → None.
        assert!(downloaded_model_dir(home).is_none());
        // Create the model dir + marker → Some(dir).
        let dir = mur_common::local_llm::local_model_dir(
            home,
            mur_common::local_llm::DEFAULT_LOCAL_MODEL_DIR,
        );
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(mur_common::local_llm::model_complete_marker(&dir), b"ok").unwrap();
        assert_eq!(downloaded_model_dir(home), Some(dir));
    }
```

(If `tempfile` is not already a dev-dependency of the Hub crate, add `tempfile = "3"` under `[dev-dependencies]` in `mur-hub-gui/src-tauri/Cargo.toml`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml downloaded_model_dir_requires_complete_marker`
Expected: FAIL — `cannot find function downloaded_model_dir`.

- [ ] **Step 3: Write minimal implementation**

Add near the top of `mlx_sidecar.rs` (after the existing `use` lines, adding `use std::path::PathBuf;`):

```rust
/// The user-downloaded model dir, only if a completed download is present
/// (`.complete` marker). This lives in the writable mur_home so it survives app
/// updates — the slim-build replacement for the bundled resource.
fn downloaded_model_dir(mur_home: &std::path::Path) -> Option<PathBuf> {
    let dir = mur_common::local_llm::local_model_dir(
        mur_home,
        mur_common::local_llm::DEFAULT_LOCAL_MODEL_DIR,
    );
    mur_common::local_llm::model_complete_marker(&dir)
        .is_file()
        .then_some(dir)
}

/// Resolve the model dir to serve: a completed download first, then the legacy
/// bundled resource (present only in dev / offline full builds).
fn resolve_model_dir(app: &AppHandle) -> Option<PathBuf> {
    if let Some(dl) = downloaded_model_dir(&crate::mur_home_path()) {
        return Some(dl);
    }
    ["resources/models/default", "models/default"]
        .iter()
        .filter_map(|rel| {
            app.path()
                .resolve(rel, tauri::path::BaseDirectory::Resource)
                .ok()
        })
        .find(|p| p.is_dir())
}
```

Replace the body of `model_available` with:

```rust
pub fn model_available(app: &AppHandle) -> bool {
    resolve_model_dir(app).is_some()
}
```

In `start`, replace the inline `let model_dir = match [...]...` block (the one that resolves the bundled resource) with:

```rust
    let model_dir = match resolve_model_dir(app) {
        Some(p) => p,
        None => {
            warn!(
                "mlx sidecar: no model available (no downloaded model and no bundled \
                 resource); skipping local inference"
            );
            return;
        }
    };
```

(Leave the rest of `start` — `mur_home`, `model_arg`, spawn, readiness — unchanged.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml downloaded_model_dir_requires_complete_marker`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
git add mur-hub-gui/src-tauri/src/mlx_sidecar.rs mur-hub-gui/src-tauri/Cargo.toml
git commit -m "feat(hub): mlx sidecar serves downloaded model dir before bundled resource"
```

---

### Task 4: Point the concierge at a registry model (`set_concierge_model_ref`)

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/seed_mur.rs`

**Interfaces:**
- Produces:
  - `pub fn set_concierge_model_ref(mur_home: &Path, alias: &str) -> std::io::Result<bool>`
  - `fn upsert_model_ref(yaml: &str, alias: &str) -> String` (pure, tested)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module of `mur-hub-gui/src-tauri/src/seed_mur.rs`:

```rust
    #[test]
    fn upsert_model_ref_inserts_before_model_block() {
        let yaml = "name: mur\nmodel:\n  provider: local\n  name: X\ntransport:\n  stdio: true\n";
        let out = upsert_model_ref(yaml, "claude_sonnet");
        assert!(out.contains("model_ref: claude_sonnet\nmodel:"), "got:\n{out}");
        // Idempotent: applying again replaces, does not duplicate.
        let again = upsert_model_ref(&out, "claude_opus");
        assert_eq!(again.matches("model_ref:").count(), 1, "got:\n{again}");
        assert!(again.contains("model_ref: claude_opus"));
    }

    #[test]
    fn upsert_model_ref_replaces_existing() {
        let yaml = "name: mur\nmodel_ref: old_alias\nmodel:\n  provider: local\n";
        let out = upsert_model_ref(yaml, "new_alias");
        assert_eq!(out.matches("model_ref:").count(), 1);
        assert!(out.contains("model_ref: new_alias"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml upsert_model_ref`
Expected: FAIL — `cannot find function upsert_model_ref`.

- [ ] **Step 3: Write minimal implementation**

Add to `seed_mur.rs` (near `rewrite_model_block`):

```rust
/// Point the stock concierge at a `~/.mur/models.yaml` registry alias by setting
/// the top-level `model_ref:` field (the runtime prefers it over the inline
/// `model:` block). Returns Ok(true) if the file changed.
pub fn set_concierge_model_ref(mur_home: &Path, alias: &str) -> std::io::Result<bool> {
    let profile_path = mur_home.join("agents").join("mur").join("profile.yaml");
    if !profile_path.is_file() {
        return Ok(false);
    }
    let original = std::fs::read_to_string(&profile_path)?;
    let out = upsert_model_ref(&original, alias);
    if out != original {
        std::fs::write(&profile_path, out)?;
        return Ok(true);
    }
    Ok(false)
}

/// Insert or replace the top-level `model_ref:` line. Replaces an existing line
/// in place; otherwise inserts one immediately before the `model:` block (or
/// appends if neither key exists). A top-level line has no leading indentation.
fn upsert_model_ref(yaml: &str, alias: &str) -> String {
    let has_existing = yaml.lines().any(|l| l.starts_with("model_ref:"));
    let mut out: Vec<String> = Vec::new();
    for line in yaml.lines() {
        if has_existing {
            if line.starts_with("model_ref:") {
                out.push(format!("model_ref: {alias}"));
            } else {
                out.push(line.to_string());
            }
        } else {
            if line.starts_with("model:") {
                out.push(format!("model_ref: {alias}"));
            }
            out.push(line.to_string());
        }
    }
    if !has_existing && !yaml.lines().any(|l| l.starts_with("model:")) {
        out.push(format!("model_ref: {alias}"));
    }
    let mut joined = out.join("\n");
    if yaml.ends_with('\n') {
        joined.push('\n');
    }
    joined
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml upsert_model_ref`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
git add mur-hub-gui/src-tauri/src/seed_mur.rs
git commit -m "feat(hub): set_concierge_model_ref to point concierge at a registry model"
```

---

### Task 5: Hub Tauri commands — download model & use registry model

**Files:**
- Create: `mur-hub-gui/src-tauri/src/model_download.rs`
- Modify: `mur-hub-gui/src-tauri/src/models_admin.rs` (add `use_registry_model`)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (declare module + register both commands in `generate_handler!`)

**Interfaces:**
- Consumes: `mur_core::model_download::download_hf_model` (Task 2), `crate::seed_mur::set_concierge_model_ref` (Task 4), `crate::mlx_sidecar::start` (Task 3), `mur_common::local_llm::{local_model_dir, DEFAULT_LOCAL_MODEL_DIR, DEFAULT_LOCAL_MODEL_REPO}`, `crate::{mur_home_path, SupervisorState}`.
- Produces: Tauri commands `download_local_model(app) -> Result<(), String>`, `use_registry_model(app, ref_name) -> Result<(), String>`. Frontend events: `model-download-progress` `{done, total}`, `model-download-done`.

> No unit test: these are thin async glue with no branch logic (right-sizing). Verified by `cargo build`.

- [ ] **Step 1: Create the download command module**

Create `mur-hub-gui/src-tauri/src/model_download.rs`:

```rust
//! First-run Tauri command: download the default local model into the writable
//! `~/.mur/models/<name>/` dir, streaming progress to the frontend, then start
//! the mlx sidecar so the concierge can use it.

use tauri::{AppHandle, Emitter};

#[tauri::command]
pub async fn download_local_model(app: AppHandle) -> Result<(), String> {
    let home = crate::mur_home_path();
    let dest =
        mur_common::local_llm::local_model_dir(&home, mur_common::local_llm::DEFAULT_LOCAL_MODEL_DIR);
    let repo = mur_common::local_llm::DEFAULT_LOCAL_MODEL_REPO;

    let app_progress = app.clone();
    mur_core::model_download::download_hf_model(repo, &dest, move |done, total| {
        let _ = app_progress.emit(
            "model-download-progress",
            serde_json::json!({ "done": done, "total": total }),
        );
    })
    .await
    .map_err(|e| format!("{e:#}"))?;

    // Model present now → start local inference and tell the UI to close the picker.
    crate::mlx_sidecar::start(&app);
    let _ = app.emit("model-download-done", ());
    Ok(())
}
```

- [ ] **Step 2: Add `use_registry_model` to `models_admin.rs`**

Append to `mur-hub-gui/src-tauri/src/models_admin.rs` (it already imports the registry types; add `use tauri::{AppHandle, Manager};` at the top if not present):

```rust
/// Point the built-in concierge at a registry model alias (set on first-run
/// "connect an LLM"), then restart it so it picks up the new backend.
#[tauri::command]
pub fn use_registry_model(app: AppHandle, ref_name: String) -> Result<(), String> {
    let home = crate::mur_home_path();
    crate::seed_mur::set_concierge_model_ref(&home, &ref_name).map_err(|e| e.to_string())?;
    let supervisor = app.state::<crate::SupervisorState>().0.clone();
    tauri::async_runtime::spawn(async move {
        supervisor.stop("mur").await; // returns ()
        if let Err(e) = supervisor.start("mur").await {
            tracing::warn!("restart concierge after model change failed: {e}");
        }
    });
    Ok(())
}
```

- [ ] **Step 3: Register module + commands in `lib.rs`**

In `mur-hub-gui/src-tauri/src/lib.rs`, add the module declaration alongside the other `mod` lines:

```rust
mod model_download;
```

Then inside `tauri::generate_handler![ ... ]` add (next to the other model commands such as `models_admin::add_models`):

```rust
            model_download::download_local_model,
            models_admin::use_registry_model,
```

- [ ] **Step 4: Build to verify it compiles**

Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: builds (warnings ok).

- [ ] **Step 5: Commit**

```bash
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
git add mur-hub-gui/src-tauri/src/model_download.rs mur-hub-gui/src-tauri/src/models_admin.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): download_local_model + use_registry_model first-run commands"
```

---

### Task 6: Slim the bundle + first-run `need-model` decision

**Files:**
- Modify: `mur-hub-gui/src-tauri/tauri.conf.json` (remove model glob)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (emit `need-model` when nothing configured)

**Interfaces:**
- Consumes: `crate::mlx_sidecar::model_available` (Task 3), `crate::seed_mur::ensure_concierge_model` (existing), `crate::mur_home_path()`.
- Produces: frontend event `need-model` (no payload) emitted on launch when no model is configured.

> No unit test: build-verified glue + manual first-run check (the decision booleans are covered by Task 3's marker test and existing `ensure_concierge_model`).

- [ ] **Step 1: Remove the model resource glob**

In `mur-hub-gui/src-tauri/tauri.conf.json`, change the `resources` array (lines ~82-85) from:

```json
    "resources": [
      "resources/mur-agent-template/**/*",
      "resources/models/**/*"
    ]
```

to:

```json
    "resources": [
      "resources/mur-agent-template/**/*"
    ]
```

(Leave `externalBin` — including `binaries/mlx-server` — untouched; the server binary is still needed to run the downloaded weights.)

- [ ] **Step 2: Emit `need-model` after the existing fallback block**

In `mur-hub-gui/src-tauri/src/lib.rs`, immediately AFTER the existing `if !mlx_sidecar::model_available(app.handle()) { ... ensure_concierge_model ... }` block (ends ~line 468) and BEFORE the "Ensure the built-in concierge's runtime is running" block (~line 470), insert:

```rust
            // First-run gate: if no usable model is configured, ask the frontend
            // to open the mandatory model picker. "Configured" means a downloaded
            // local model is present, OR the concierge profile already points at a
            // non-stock backend (a `model_ref:` cloud entry, or an ollama fallback
            // that flipped `provider:` away from the stock `local`).
            {
                let home = mur_home_path();
                let ready = mlx_sidecar::model_available(app.handle())
                    || std::fs::read_to_string(home.join("agents").join("mur").join("profile.yaml"))
                        .map(|s| s.contains("model_ref:") || !s.contains("provider: local"))
                        .unwrap_or(false);
                if !ready {
                    let _ = app.handle().emit("need-model", ());
                    tracing::info!("first-run: no model configured; requested model picker");
                }
            }
```

(`emit` needs the `Emitter` trait — `lib.rs` already calls `app.emit(...)` elsewhere, so it's in scope.)

- [ ] **Step 3: Build to verify it compiles**

Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: builds.

- [ ] **Step 4: Manual smoke check (slim bundle path)**

With a throwaway home: `MUR_HOME=$(mktemp -d) cargo run --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (dev run). Confirm the log line `first-run: no model configured; requested model picker` appears when no ollama/oMLX is running and no model is downloaded. (Full DMG verification is in Task 7's notes.)

- [ ] **Step 5: Commit**

```bash
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
git add mur-hub-gui/src-tauri/tauri.conf.json mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): drop bundled model from DMG; emit need-model on first run"
```

---

### Task 7: First-run picker UI (mur-web frontend)

**Files (in the separate `~/Projects/mur-web` repo):**
- The Model Library component + its container (find via `grep -rln "add_models\|probe_local_providers" ~/Projects/mur-web/src`).
- The app shell that mounts modals / listens to Tauri events.

**Interfaces (contract this UI must satisfy — all defined in Tasks 5–6):**
- Listen for Tauri event `need-model` → open the Model Library as a **mandatory, non-dismissible** modal (no close button, no backdrop-dismiss; the rest of the app is not interactable).
- Add one tile at the top: **"Use MUR's local model — download ~1.6GB"**. On click → `invoke('download_local_model')`. While running, subscribe to `model-download-progress` (`{done, total}`) and render a progress bar (if `total === 0`, show bytes-downloaded / indeterminate). On `model-download-done` (or the invoke resolving) → close the modal.
- The existing connect-cloud / connect-local tiles already call `add_models`. After a model is added and the user picks one for the concierge → `invoke('use_registry_model', { refName })` → on resolve, close the modal.
- On download error (invoke rejects), show the message + a **Retry** button; keep the connect tiles available so the user can choose a cloud/local LLM instead.

> No exact JSX here on purpose — the frontend lives in another repo with its own component conventions. **Step 1 is to read those conventions first.** The contract above is fixed; match it to mur-web's existing modal + `invoke`/`listen` patterns.

- [ ] **Step 1: Read mur-web conventions**

Run: `grep -rln "useEffect\|listen(\|invoke(" ~/Projects/mur-web/src | head` and open the Model Library component + the existing wizard/onboarding modal (it already gates first run — mirror its mandatory pattern). Note how events are subscribed (`@tauri-apps/api/event` `listen`) and how `invoke` is imported.

- [ ] **Step 2: Add the `need-model` listener + mandatory modal**

In the app shell, `listen('need-model', () => setShowModelPicker(true))`. Render the Model Library inside a non-dismissible modal when `showModelPicker` is true. Match the existing onboarding modal's "cannot dismiss" approach.

- [ ] **Step 3: Add the download tile + progress**

In the Model Library, add the top tile that calls `invoke('download_local_model')`, subscribes to `model-download-progress` for the bar, and closes on `model-download-done`. Add the error + Retry state.

- [ ] **Step 4: Wire the connect tiles to the concierge**

After the existing add-model flow, call `invoke('use_registry_model', { refName })` with the chosen alias, then close the modal.

- [ ] **Step 5: Build the frontend & embed**

Run: `cd ~/Projects/mur-web && npm run build`. Then build the Hub with the embedded dist per `CLAUDE.md` (`MUR_WEB_DIST=$HOME/Projects/mur-web/dist` … or the Hub's own bundling step). Manually verify: fresh `MUR_HOME`, no local runtime → picker appears → download completes → concierge chats; connect-cloud path sets `model_ref` and chats; second launch (model cached) → no picker.

- [ ] **Step 6: Commit (in mur-web)**

```bash
cd ~/Projects/mur-web
git add -A
git commit -m "feat: first-run model picker (download local model or connect an LLM)"
```

---

## Final verification (after all tasks)

- [ ] Workspace lint/test: `ORT_STRATEGY=download cargo nextest run -p mur-common -p mur-core` and `cargo clippy -p mur-common -p mur-core -- -D warnings`.
- [ ] Hub crate: `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml --check`, `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- -D warnings`, `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml`.
- [ ] **DMG size:** build the Hub `.dmg` and confirm it dropped by ~1.6GB and that `MUR Hub.app/Contents/Resources/resources/models/` is absent.
- [ ] **Cache survives update:** download the model, "reinstall" (rebuild + reopen) → no re-download (picker does not reappear; `~/.mur/models/Qwen3.5-2B-MLX-4bit/.complete` still present).
