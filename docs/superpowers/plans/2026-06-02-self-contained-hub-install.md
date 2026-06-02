# Self-Contained MuR Hub — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship one signed macOS `.dmg` whose `MuR Hub.app` embeds `mur` + `mur-agent-runtime` + a local MLX model + a seed "Mur" concierge agent, so a non-developer can double-click, open, and immediately chat with an offline Chinese-speaking agent — plus a one-click model-upgrade path.

**Architecture:** Extend the existing `mur-hub-gui` Tauri app. The agent runtime is already bundled-resolvable as an `externalBin` sidecar. Add a second sidecar (an MLX OpenAI-compatible inference server), a small `local` provider in the runtime that talks to it key-lessly, a shared file that carries the server's ephemeral base URL between Hub and launchd-managed agents, idempotent seeding of the "Mur" agent from a bundled template, a CLI-tools install menu, and a release job that builds/signs/notarizes the `.dmg`.

**Tech Stack:** Rust (mur-core, mur-common, mur-agent-runtime, mur-gui-core), Tauri 2 + `tauri-plugin-shell` sidecars, frozen `mlx-lm` OpenAI-compatible server, GitHub Actions (codesign + notarytool).

**Key implementation decisions (resolving spec §15 / §6 open questions):**
- **MLX sidecar = frozen `mlx-lm` server** (`mlx_lm.server`, OpenAI-compatible), bundled as `externalBin` named `mlx-server`. Native MLX-Swift server is deferred (future optimization). Rationale: concrete, mature, testable at the HTTP boundary; the user never sees Python because it is frozen into one binary.
- **Local provider = new `"local"` arm** in the runtime, building the existing `OpenAiClient` against the sidecar with a constant non-secret placeholder key. No API key required.
- **Base-URL transport = a file** `~/.mur/runtime/local_llm.url` (Hub writes, runtime reads). Chosen because agents run under launchd and do **not** inherit Hub's process env. No hardcoded port.
- **Known v1 limitation (documented, not fixed here):** the MLX sidecar's lifetime is tied to Hub. Agents kept running by launchd after Hub quits lose the local brain until Hub reopens; the upgrade path (cloud/local-Ollama model) covers always-on use.

---

## File Structure

**Create:**
- `mur-common/src/local_llm.rs` — shared path + read/write for the local model base URL.
- `mur-hub-gui/src-tauri/src/mlx_sidecar.rs` — spawn/supervise the MLX server sidecar; port + health helpers.
- `mur-hub-gui/src-tauri/src/seed_mur.rs` — idempotent seeding of the "Mur" agent from the bundled template.
- `mur-hub-gui/src-tauri/src/cli_tools.rs` — "Install command-line tools" command.
- `mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml` — seed Mur profile.
- `mur-hub-gui/src-tauri/resources/mur-agent-template/sys_prompt.md` — seed Mur system prompt.
- `mur-hub-gui/src-tauri/resources/mur-agent-template/skills/concierge.yaml` — concierge skill manifest.
- `scripts/build-mlx-server.sh` — freeze `mlx-lm` into the `mlx-server` sidecar binary.
- `scripts/fetch-bundle-model.sh` — download the default model into the resources dir (not committed).

**Modify:**
- `mur-common/src/lib.rs` — register `pub mod local_llm;`.
- `mur-common/src/config.rs` — add the default bundled-model id constant/config field.
- `mur-agent-runtime/src/supervisor_runner.rs` — add the `"local"` provider arm.
- `mur-core/src/cmd/agent/mod.rs` — add app-bundle branch to `resolve_runtime_target()`.
- `mur-hub-gui/src-tauri/src/lib.rs` — register modules, commands, MLX-sidecar startup, seed-on-first-launch, CLI-tools menu item, brain-badge command.
- `mur-hub-gui/src-tauri/tauri.conf.json` — `externalBin` + `resources` + dmg.
- `mur-hub-gui/src-tauri/Cargo.toml` — add `mur-common`, `reqwest` (if absent) deps.
- `.github/workflows/release.yml` — add the Hub build/sign/notarize/upload job.
- `README.md` — add the macOS Hub download to Quick Start.

---

## Phase 1 — Runtime plumbing (pure Rust, TDD)

### Task 1: Shared local-LLM base-URL file (`mur-common`)

**Files:**
- Create: `mur-common/src/local_llm.rs`
- Modify: `mur-common/src/lib.rs`
- Test: in `mur-common/src/local_llm.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

```rust
// mur-common/src/local_llm.rs
//! Location and accessors for the bundled local-model base URL.
//!
//! Hub starts an MLX inference server on an ephemeral port and writes its
//! OpenAI-compatible base URL here. Agents — started by launchd and therefore
//! NOT inheriting Hub's environment — read it back from this file.

use std::path::{Path, PathBuf};

/// Path to the file holding the local model base URL, under `<mur_home>`.
pub fn base_url_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("local_llm.url")
}

/// Atomically write the base URL (temp file + rename).
pub fn write_base_url(mur_home: &Path, url: &str) -> std::io::Result<()> {
    let path = base_url_path(mur_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("url.tmp");
    std::fs::write(&tmp, url.as_bytes())?;
    std::fs::rename(&tmp, &path)
}

/// Read the base URL, trimming whitespace. `None` if absent/empty.
pub fn read_base_url(mur_home: &Path) -> Option<String> {
    let s = std::fs::read_to_string(base_url_path(mur_home)).ok()?;
    let t = s.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_then_read_roundtrips() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read_base_url(tmp.path()), None);
        write_base_url(tmp.path(), "http://127.0.0.1:50321/v1").unwrap();
        assert_eq!(
            read_base_url(tmp.path()),
            Some("http://127.0.0.1:50321/v1".to_string())
        );
    }

    #[test]
    fn blank_file_reads_as_none() {
        let tmp = TempDir::new().unwrap();
        write_base_url(tmp.path(), "   \n").unwrap();
        assert_eq!(read_base_url(tmp.path()), None);
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-common/src/lib.rs`, add alongside the other `pub mod` lines:

```rust
pub mod local_llm;
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p mur-common local_llm`
Expected: PASS (2 tests). (`tempfile` is already a dev-dependency in this crate; if the run reports it missing, add `tempfile` under `[dev-dependencies]` in `mur-common/Cargo.toml` and re-run.)

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/local_llm.rs mur-common/src/lib.rs
git commit -m "feat(common): shared local-model base-URL file accessors"
```

---

### Task 2: Default bundled-model id in config

**Files:**
- Modify: `mur-common/src/config.rs`
- Test: in `mur-common/src/config.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add near the other tests in `mur-common/src/config.rs`:

```rust
#[test]
fn default_bundled_model_id_is_qwen35_2b() {
    assert_eq!(
        crate::config::DEFAULT_BUNDLED_MODEL_ID,
        "Qwen3.5-2B-MLX-4bit"
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p mur-common default_bundled_model_id`
Expected: FAIL — `DEFAULT_BUNDLED_MODEL_ID` not found.

- [ ] **Step 3: Add the constant**

Near the top of `mur-common/src/config.rs` (after the imports), add:

```rust
/// Default model id seeded for the built-in "Mur" agent and used to name the
/// bundled MLX weights. This is the DEFAULT VALUE only — it is written into the
/// seed agent's profile and can be changed by the user afterwards; it is not a
/// behavioural constant baked into logic.
pub const DEFAULT_BUNDLED_MODEL_ID: &str = "Qwen3.5-2B-MLX-4bit";
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p mur-common default_bundled_model_id`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(common): add DEFAULT_BUNDLED_MODEL_ID"
```

---

### Task 3: `local` provider arm in the runtime

**Files:**
- Modify: `mur-agent-runtime/src/supervisor_runner.rs:112` (the `match entry.provider.as_str()` block)
- Test: in `mur-agent-runtime/src/supervisor_runner.rs` (`#[cfg(test)]`)

Context: the existing `match` (read at `supervisor_runner.rs:112`) has arms for `"ollama"`, `"anthropic"`, `"openai"`. We add a `"local"` arm that builds an `OpenAiClient` pointed at the sidecar with a placeholder key, resolving the base URL from `entry.base_url` → env `MUR_LOCAL_LLM_BASE_URL` → the shared file (Task 1) → a final default.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module of `supervisor_runner.rs`:

```rust
#[test]
fn local_base_url_prefers_entry_then_env_then_file_then_default() {
    use std::path::Path;
    // entry wins
    assert_eq!(
        super::resolve_local_base_url(Some("http://e/v1"), None, Path::new("/nonexistent")),
        "http://e/v1"
    );
    // env wins when entry absent
    assert_eq!(
        super::resolve_local_base_url(None, Some("http://env/v1".into()), Path::new("/nonexistent")),
        "http://env/v1"
    );
    // default when nothing available
    assert_eq!(
        super::resolve_local_base_url(None, None, Path::new("/nonexistent")),
        super::LOCAL_LLM_DEFAULT_BASE_URL
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p mur-agent-runtime local_base_url_prefers`
Expected: FAIL — `resolve_local_base_url` / `LOCAL_LLM_DEFAULT_BASE_URL` not found.

- [ ] **Step 3: Add the resolver helper**

Near the top of `supervisor_runner.rs` (after the `use` lines), add:

```rust
/// Fallback base URL when neither the registry entry, the env var, nor the
/// shared file provides one (e.g. running outside Hub). Points at the
/// conventional local sidecar port.
pub(crate) const LOCAL_LLM_DEFAULT_BASE_URL: &str = "http://127.0.0.1:50320/v1";

/// Placeholder API key for the local OpenAI-compatible MLX server, which does
/// not authenticate. Not a secret.
pub(crate) const LOCAL_LLM_PLACEHOLDER_KEY: &str = "local-no-key";

/// Resolve the local model base URL: entry.base_url → env → shared file → default.
pub(crate) fn resolve_local_base_url(
    entry_base_url: Option<&str>,
    env_base_url: Option<String>,
    mur_home: &std::path::Path,
) -> String {
    if let Some(u) = entry_base_url {
        return u.to_string();
    }
    if let Some(u) = env_base_url {
        return u;
    }
    if let Some(u) = mur_common::local_llm::read_base_url(mur_home) {
        return u;
    }
    LOCAL_LLM_DEFAULT_BASE_URL.to_string()
}
```

- [ ] **Step 4: Add the `"local"` arm**

Inside the `match entry.provider.as_str()` block (immediately before the `"ollama"` arm at `supervisor_runner.rs:113`), add:

```rust
        "local" => {
            let mur_home = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".mur");
            let base = resolve_local_base_url(
                entry.base_url.as_deref(),
                std::env::var("MUR_LOCAL_LLM_BASE_URL").ok(),
                &mur_home,
            );
            let key = secrecy::SecretString::from(LOCAL_LLM_PLACEHOLDER_KEY.to_string());
            let client = Arc::new(OpenAiClient::from_secret_string_with_http(
                &key,
                entry.model.clone(),
                Some(base),
                guarded_http,
            ));
            build(client)
        }
```

(If `secrecy` is not already imported in this file, add `use secrecy::SecretString;` and use `SecretString::from(...)`. Confirm the `OpenAiClient::from_secret_string_with_http` signature matches the `"openai"` arm at `supervisor_runner.rs:152` — it takes `&SecretString, String, Option<String>, reqwest::Client`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-agent-runtime local_base_url_prefers`
Expected: PASS.
Run: `cargo build -p mur-agent-runtime`
Expected: builds cleanly.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/supervisor_runner.rs
git commit -m "feat(runtime): add key-less 'local' provider for bundled MLX server"
```

---

### Task 4: App-bundle branch in `resolve_runtime_target()`

**Files:**
- Modify: `mur-core/src/cmd/agent/mod.rs:120` (`resolve_runtime_target`)
- Test: in `mur-core/src/cmd/agent/mod.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module of `mur-core/src/cmd/agent/mod.rs`:

```rust
#[test]
fn bundle_runtime_resolves_from_macos_dir() {
    use std::path::Path;
    // Hub exe inside a .app → runtime sibling in Contents/MacOS.
    let exe = Path::new("/Applications/MuR Hub.app/Contents/MacOS/mur-hub-gui");
    let got = super::runtime_target_in_bundle(exe, "mur-agent-runtime");
    assert_eq!(
        got.as_deref(),
        Some(Path::new(
            "/Applications/MuR Hub.app/Contents/MacOS/mur-agent-runtime"
        ))
    );
}

#[test]
fn non_bundle_path_returns_none() {
    use std::path::Path;
    let exe = Path::new("/opt/homebrew/bin/mur");
    assert_eq!(super::runtime_target_in_bundle(exe, "mur-agent-runtime"), None);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p mur-core runtime_target_in_bundle`
Expected: FAIL — `runtime_target_in_bundle` not found. (Also `non_bundle_path_returns_none` references it.)

- [ ] **Step 3: Add the pure helper**

In `mur-core/src/cmd/agent/mod.rs`, just above `resolve_runtime_target` (currently at line 120), add:

```rust
/// If `exe` lives inside a macOS `.app` bundle's `Contents/MacOS` directory,
/// return the sibling runtime path. Returns `None` otherwise. Pure (testable).
pub(crate) fn runtime_target_in_bundle(
    exe: &std::path::Path,
    runtime_filename: &str,
) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if dir.file_name().and_then(|s| s.to_str()) != Some("MacOS") {
        return None;
    }
    if dir.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()) != Some("Contents") {
        return None;
    }
    Some(dir.join(runtime_filename))
}
```

- [ ] **Step 4: Wire the helper into `resolve_runtime_target`**

In `resolve_runtime_target` (line 120+), insert the new branch **after** the `MUR_AGENT_RUNTIME_BIN` env check and **before** the existing `current_exe` "next-to" check. The function currently reads (see `mod.rs:120-138`):

```rust
pub(crate) fn resolve_runtime_target() -> PathBuf {
    if let Some(v) = std::env::var_os("MUR_AGENT_RUNTIME_BIN") {
        return PathBuf::from(v);
    }
    let runtime_filename = if cfg!(windows) {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    };
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(runtime_filename);
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from(runtime_filename)
}
```

Change the `if let Ok(exe) = std::env::current_exe()` block to first try the bundle branch:

```rust
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bundle) = runtime_target_in_bundle(&exe, runtime_filename) {
            if bundle.exists() {
                return bundle;
            }
        }
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(runtime_filename);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from(runtime_filename)
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-core runtime_target_in_bundle`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/mod.rs
git commit -m "feat(core): resolve bundled runtime from inside .app for CLI path"
```

---

## Phase 2 — MLX sidecar (Hub)

### Task 5: MLX sidecar port + health helpers (pure)

**Files:**
- Create: `mur-hub-gui/src-tauri/src/mlx_sidecar.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (add `pub mod mlx_sidecar;`)
- Test: in `mur-hub-gui/src-tauri/src/mlx_sidecar.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test + pure helpers**

Create `mur-hub-gui/src-tauri/src/mlx_sidecar.rs`:

```rust
//! MLX inference sidecar — spawns the bundled `mlx-server` (frozen mlx-lm,
//! OpenAI-compatible) on an ephemeral port and publishes its base URL via the
//! shared file so launchd-managed agents can reach it.

use std::net::TcpListener;

/// Reserve a free localhost TCP port by binding to :0 and reading the assigned
/// port. The listener is dropped immediately; a tiny race window exists before
/// the sidecar binds, which is acceptable here.
pub fn pick_free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// OpenAI-compatible base URL for the sidecar on `port`.
pub fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1")
}

/// Readiness probe URL (returns 200 once the model is loaded).
pub fn health_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1/models")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_free_port_is_nonzero() {
        let p = pick_free_port().unwrap();
        assert!(p > 0);
    }

    #[test]
    fn url_helpers_format_correctly() {
        assert_eq!(base_url(50320), "http://127.0.0.1:50320/v1");
        assert_eq!(health_url(50320), "http://127.0.0.1:50320/v1/models");
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-hub-gui/src-tauri/src/lib.rs`, add to the module list near the top (after `pub mod companion;`):

```rust
pub mod mlx_sidecar;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p mur-hub-gui --manifest-path mur-hub-gui/src-tauri/Cargo.toml mlx_sidecar`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mlx_sidecar.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): MLX sidecar port + URL helpers"
```

---

### Task 6: Spawn + supervise the MLX sidecar, publish base URL

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mlx_sidecar.rs` (add spawn fn)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (call on setup; add `mur-common` dep usage)
- Modify: `mur-hub-gui/src-tauri/Cargo.toml` (ensure `mur-common` dep)

- [ ] **Step 1: Add the spawn function**

Append to `mur-hub-gui/src-tauri/src/mlx_sidecar.rs`:

```rust
use tauri::{AppHandle, Manager};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::CommandEvent;
use tracing::{info, warn};

/// Start the bundled `mlx-server` sidecar against the bundled model, write its
/// base URL to the shared file, and stream its logs. Idempotent at the
/// application level (call once on setup). Errors are logged, not fatal: if MLX
/// can't start, agents fall back to echo/cloud providers.
pub fn start(app: &AppHandle) {
    let port = match pick_free_port() {
        Ok(p) => p,
        Err(e) => {
            warn!("mlx sidecar: no free port: {e}");
            return;
        }
    };

    // Resolve the bundled model directory from app resources.
    let model_dir = match app
        .path()
        .resolve("models/default", tauri::path::BaseDirectory::Resource)
    {
        Ok(p) => p,
        Err(e) => {
            warn!("mlx sidecar: cannot resolve model resource: {e}");
            return;
        }
    };

    // Publish base URL for launchd-managed agents (Task 1 helper).
    let mur_home = crate::mur_home_path();
    if let Err(e) = mur_common::local_llm::write_base_url(&mur_home, &base_url(port)) {
        warn!("mlx sidecar: failed to write base url: {e}");
    }

    let cmd = app
        .shell()
        .sidecar("mlx-server")
        .and_then(|c| {
            Ok(c.args([
                "--model",
                model_dir.to_str().unwrap_or_default(),
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ]))
        });
    let cmd = match cmd {
        Ok(c) => c,
        Err(e) => {
            warn!("mlx sidecar: cannot create command: {e}");
            return;
        }
    };

    match cmd.spawn() {
        Ok((mut rx, _child)) => {
            info!(port, "mlx sidecar spawned");
            tauri::async_runtime::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    if let CommandEvent::Stderr(line) = ev {
                        info!("mlx-server: {}", String::from_utf8_lossy(&line).trim());
                    }
                }
            });
        }
        Err(e) => warn!("mlx sidecar: spawn failed: {e}"),
    }
}
```

- [ ] **Step 2: Ensure `mur-common` is a dependency**

In `mur-hub-gui/src-tauri/Cargo.toml`, confirm under `[dependencies]` there is a `mur-common` line. If absent, add (match the workspace path style used by sibling crates):

```toml
mur-common = { path = "../../mur-common" }
```

- [ ] **Step 3: Call `mlx_sidecar::start` during setup**

In `mur-hub-gui/src-tauri/src/lib.rs`, inside the `.setup(move |app| { ... })` closure (after the discovery wiring, before `Ok(())` at line ~293), add:

```rust
            // Start the bundled local inference server (best-effort).
            mlx_sidecar::start(app.handle());
```

- [ ] **Step 4: Verify it builds**

Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: builds. (It will only actually spawn at runtime when the `mlx-server` sidecar and model resource exist in a bundle; in dev it logs a warning and continues.)

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mlx_sidecar.rs mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/Cargo.toml
git commit -m "feat(hub): spawn MLX sidecar and publish its base URL"
```

---

## Phase 3 — Seed "Mur" agent

### Task 7: Bundled "Mur" template assets

**Files:**
- Create: `mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml`
- Create: `mur-hub-gui/src-tauri/resources/mur-agent-template/sys_prompt.md`
- Create: `mur-hub-gui/src-tauri/resources/mur-agent-template/skills/concierge.yaml`

- [ ] **Step 1: Write the profile**

Create `mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml`. Match the field shape produced by `mur-core/src/cmd/agent/lifecycle.rs` (see the `AgentProfile` written there) — the critical part is the `model` block using the new `local` provider:

```yaml
name: Mur
model:
  provider: local
  name: Qwen3.5-2B-MLX-4bit
  params:
    temperature: 0.7
    top_p: 0.8
    max_tokens: 1024
```

(Only fields needed to launch are required; the runtime fills the rest from defaults. If `mur agent create` writes additional mandatory fields, mirror them here verbatim so the profile deserializes — verify by deserializing in Task 8's test.)

- [ ] **Step 2: Write the system prompt**

Create `mur-hub-gui/src-tauri/resources/mur-agent-template/sys_prompt.md`:

```markdown
# Mur — your guide to MuR

You are **Mur**, the friendly built-in guide for the MuR Hub app. You run
entirely on the user's machine, offline and private.

Speak warmly and concisely. Default to the user's language; when they write in
Chinese, reply in Traditional Chinese (zh-TW).

Your job on first meeting:
1. Greet the user warmly and say, in one line, what MuR is: a local-first place
   to create and run your own AI agents.
2. Offer to help them create their first agent, or to connect a more capable
   model when they want you to be smarter.
3. If asked something your small local brain struggles with, say so kindly and
   offer the upgrade — never nag.
```

- [ ] **Step 3: Write the concierge skill manifest**

Create `mur-hub-gui/src-tauri/resources/mur-agent-template/skills/concierge.yaml`. Mirror the structure of an existing manifest (`mur-core/src/skills/mur_project_search.yaml`):

```yaml
name: concierge
description: >
  First-run guidance for new MuR users. Use when the user is new, asks what MuR
  is, asks how to start, or wants to create their first agent.
triggers:
  - "what is mur"
  - "get started"
  - "create an agent"
content:
  technical: >
    Walk the user through creating their first agent and, when they want more
    capability, connecting a larger model via the model wizard.
  principle: >
    Be warm and brief. Never pressure the user to upgrade; offer it only when
    the local model genuinely falls short of what they asked for.
```

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/resources/mur-agent-template
git commit -m "feat(hub): bundled 'Mur' concierge agent template"
```

---

### Task 8: Idempotent seeding logic (pure) + Tauri wiring

**Files:**
- Create: `mur-hub-gui/src-tauri/src/seed_mur.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (module + call on first launch)
- Test: in `mur-hub-gui/src-tauri/src/seed_mur.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test + pure copy logic**

Create `mur-hub-gui/src-tauri/src/seed_mur.rs`:

```rust
//! Seed the built-in "Mur" agent from the bundled template on first launch.
//!
//! Idempotent: seeds only when NO agents exist under `<mur_home>/agents`, so it
//! never clobbers a user who already has agents or deleted Mur on purpose.

use std::path::Path;

/// True if any agent directory already exists under `<mur_home>/agents`.
pub fn any_agent_exists(mur_home: &Path) -> bool {
    let agents = mur_home.join("agents");
    match std::fs::read_dir(&agents) {
        Ok(mut entries) => entries.any(|e| e.map(|e| e.path().is_dir()).unwrap_or(false)),
        Err(_) => false,
    }
}

/// Recursively copy `src` into `dst`.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Seed Mur from `template_dir` into `<mur_home>/agents/mur` iff no agents
/// exist. Returns Ok(true) if seeding happened, Ok(false) if skipped.
pub fn seed_if_empty(template_dir: &Path, mur_home: &Path) -> std::io::Result<bool> {
    if any_agent_exists(mur_home) {
        return Ok(false);
    }
    let dst = mur_home.join("agents").join("mur");
    copy_tree(template_dir, &dst)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_template(dir: &Path) {
        std::fs::create_dir_all(dir.join("skills")).unwrap();
        std::fs::write(dir.join("profile.yaml"), "name: Mur\n").unwrap();
        std::fs::write(dir.join("sys_prompt.md"), "# Mur\n").unwrap();
        std::fs::write(dir.join("skills/concierge.yaml"), "name: concierge\n").unwrap();
    }

    #[test]
    fn seeds_when_empty() {
        let home = TempDir::new().unwrap();
        let tpl = TempDir::new().unwrap();
        make_template(tpl.path());
        assert!(seed_if_empty(tpl.path(), home.path()).unwrap());
        assert!(home.path().join("agents/mur/profile.yaml").exists());
        assert!(home.path().join("agents/mur/skills/concierge.yaml").exists());
    }

    #[test]
    fn skips_when_an_agent_exists() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("agents/other")).unwrap();
        let tpl = TempDir::new().unwrap();
        make_template(tpl.path());
        assert!(!seed_if_empty(tpl.path(), home.path()).unwrap());
        assert!(!home.path().join("agents/mur").exists());
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml seed_mur`
Expected: PASS (2 tests). (If `tempfile` is not a dev-dependency of the Hub crate, add it under `[dev-dependencies]` in `mur-hub-gui/src-tauri/Cargo.toml`.)

- [ ] **Step 3: Verify the bundled profile deserializes**

Add this test to `seed_mur.rs` to guard the real template against drift, then run it:

```rust
    #[test]
    fn bundled_profile_deserializes() {
        // The real template lives next to the crate; ensure it parses as a profile.
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/mur-agent-template/profile.yaml"
        );
        let body = std::fs::read_to_string(p).unwrap();
        let _profile: mur_common::agent::AgentProfile =
            serde_yaml_ng::from_str(&body).expect("seed profile must deserialize");
    }
```

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml bundled_profile_deserializes`
Expected: PASS. If it fails, add the missing mandatory fields (reported by the serde error) to `resources/mur-agent-template/profile.yaml` until it deserializes, then re-run. (`mur-common` and `serde_yaml_ng` must be dependencies of the Hub crate; add them if the test won't compile.)

- [ ] **Step 4: Register the module and seed on first launch**

In `mur-hub-gui/src-tauri/src/lib.rs` add `pub mod seed_mur;` to the module list, then inside `.setup(...)` (right before `mlx_sidecar::start(...)` from Task 6) add:

```rust
            // Seed the built-in "Mur" agent on first run (idempotent).
            if let Ok(template_dir) = app.path().resolve(
                "mur-agent-template",
                tauri::path::BaseDirectory::Resource,
            ) {
                let mur_home = mur_home_path();
                match seed_mur::seed_if_empty(&template_dir, &mur_home) {
                    Ok(true) => {
                        tracing::info!("seeded built-in Mur agent");
                        // Create the runtime symlink + start via the supervisor.
                        let supervisor = app.state::<SupervisorState>();
                        let handle = supervisor.0.clone();
                        tauri::async_runtime::spawn(async move {
                            handle.start("mur").await;
                        });
                    }
                    Ok(false) => {}
                    Err(e) => tracing::warn!("seed Mur failed: {e}"),
                }
            }
```

Note: `Supervisor` is cloneable via its handle channel; if `supervisor.0.clone()` does not compile, capture the `Supervisor` reference and call `supervisor.0.start("mur").await` inside a `tauri::async_runtime::block_on` instead, matching how `start_agent` (lib.rs:46) invokes it.

- [ ] **Step 5: Verify it builds**

Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: builds.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/src-tauri/src/seed_mur.rs mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/Cargo.toml
git commit -m "feat(hub): idempotently seed built-in Mur agent on first launch"
```

---

## Phase 4 — Bundling

### Task 9: Tauri bundle config (externalBin + resources + dmg)

**Files:**
- Modify: `mur-hub-gui/src-tauri/tauri.conf.json`
- Create (placeholder dirs so the build resolves in dev): `mur-hub-gui/src-tauri/binaries/.gitkeep`, `mur-hub-gui/src-tauri/resources/models/.gitkeep`

- [ ] **Step 1: Extend the bundle block**

In `mur-hub-gui/src-tauri/tauri.conf.json`, the `"bundle"` object currently has `active`, `targets`, `fileAssociations`, `icon`, `macOS`, `windows`. Add `externalBin` and `resources` keys:

```json
    "externalBin": [
      "binaries/mur",
      "binaries/mur-agent-runtime",
      "binaries/mlx-server"
    ],
    "resources": [
      "resources/mur-agent-template/**/*",
      "resources/models/**/*"
    ],
```

Keep the existing `"targets": ["app", "dmg"]` and the `.muragent` `fileAssociations` entry unchanged.

- [ ] **Step 2: Add keep-files so resource globs resolve in dev**

```bash
mkdir -p mur-hub-gui/src-tauri/binaries mur-hub-gui/src-tauri/resources/models
touch mur-hub-gui/src-tauri/binaries/.gitkeep mur-hub-gui/src-tauri/resources/models/.gitkeep
```

- [ ] **Step 3: Document the externalBin naming requirement**

Tauri `externalBin` entries must have the target-triple suffix at bundle time (e.g. `mur-agent-runtime-aarch64-apple-darwin`). Add a short note to `mur-hub-gui/README.md` under a new "Bundling" heading:

```markdown
## Bundling (self-contained .app)

`tauri.conf.json` embeds three sidecars from `src-tauri/binaries/` —
`mur`, `mur-agent-runtime`, `mlx-server` — each suffixed with the target triple
(e.g. `mur-agent-runtime-aarch64-apple-darwin`), and the default model under
`src-tauri/resources/models/default/`. The release workflow populates these;
see `scripts/build-mlx-server.sh` and `scripts/fetch-bundle-model.sh`.
```

- [ ] **Step 4: Validate JSON**

Run: `python3 -m json.tool mur-hub-gui/src-tauri/tauri.conf.json > /dev/null && echo OK`
Expected: `OK`.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/tauri.conf.json mur-hub-gui/src-tauri/binaries/.gitkeep mur-hub-gui/src-tauri/resources/models/.gitkeep mur-hub-gui/README.md
git commit -m "feat(hub): embed mur/runtime/mlx-server + model + template in bundle"
```

---

### Task 10: Sidecar freeze + model fetch scripts

**Files:**
- Create: `scripts/build-mlx-server.sh`
- Create: `scripts/fetch-bundle-model.sh`

- [ ] **Step 1: Write the MLX server freeze script**

Create `scripts/build-mlx-server.sh`:

```bash
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
```

(If `mlx_lm.server`'s entrypoint differs from `main`, adjust the entry script to call the documented server entrypoint. Verify with `python3 -c "import mlx_lm.server"`.)

- [ ] **Step 2: Write the model fetch script**

Create `scripts/fetch-bundle-model.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
# Download the default bundled MLX model into the resources dir. NOT committed.
# Model id mirrors DEFAULT_BUNDLED_MODEL_ID in mur-common.

MODEL_REPO="${1:-mlx-community/Qwen3.5-2B-MLX-4bit}"
DEST="mur-hub-gui/src-tauri/resources/models/default"
mkdir -p "$DEST"

python3 -m pip install --quiet huggingface_hub
python3 - "$MODEL_REPO" "$DEST" <<'PY'
import sys
from huggingface_hub import snapshot_download
repo, dest = sys.argv[1], sys.argv[2]
snapshot_download(repo_id=repo, local_dir=dest, local_dir_use_symlinks=False)
print(f"Downloaded {repo} -> {dest}")
PY
```

- [ ] **Step 3: Make executable and sanity-check syntax**

```bash
chmod +x scripts/build-mlx-server.sh scripts/fetch-bundle-model.sh
bash -n scripts/build-mlx-server.sh && bash -n scripts/fetch-bundle-model.sh && echo OK
```
Expected: `OK`.

- [ ] **Step 4: Ensure model weights stay out of git**

Append to `.gitignore`:

```
mur-hub-gui/src-tauri/resources/models/default/
mur-hub-gui/src-tauri/binaries/mlx-server-*
mur-hub-gui/src-tauri/binaries/mur-*
```

- [ ] **Step 5: Commit**

```bash
git add scripts/build-mlx-server.sh scripts/fetch-bundle-model.sh .gitignore
git commit -m "build: scripts to freeze mlx-server and fetch the bundled model"
```

---

## Phase 5 — CLI tools menu

### Task 11: "Install command-line tools" command + menu item

**Files:**
- Create: `mur-hub-gui/src-tauri/src/cli_tools.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (module, command registration, tray menu item)
- Test: in `mur-hub-gui/src-tauri/src/cli_tools.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test + pure target logic**

Create `mur-hub-gui/src-tauri/src/cli_tools.rs`:

```rust
//! "Install command-line tools" — symlink the bundled `mur` into a PATH dir.

use std::path::{Path, PathBuf};

/// Preferred PATH install dir: /opt/homebrew/bin if writable, else ~/.local/bin.
pub fn install_dir(homebrew_writable: bool, home: &Path) -> PathBuf {
    if homebrew_writable {
        PathBuf::from("/opt/homebrew/bin")
    } else {
        home.join(".local/bin")
    }
}

/// Bundled `mur` path given the Hub executable path (sibling in Contents/MacOS).
pub fn bundled_mur_path(hub_exe: &Path) -> Option<PathBuf> {
    Some(hub_exe.parent()?.join("mur"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_homebrew_when_writable() {
        assert_eq!(
            install_dir(true, Path::new("/Users/x")),
            PathBuf::from("/opt/homebrew/bin")
        );
    }

    #[test]
    fn falls_back_to_local_bin() {
        assert_eq!(
            install_dir(false, Path::new("/Users/x")),
            PathBuf::from("/Users/x/.local/bin")
        );
    }

    #[test]
    fn bundled_mur_is_sibling_of_hub() {
        let exe = Path::new("/Applications/MuR Hub.app/Contents/MacOS/mur-hub-gui");
        assert_eq!(
            bundled_mur_path(exe).unwrap(),
            PathBuf::from("/Applications/MuR Hub.app/Contents/MacOS/mur")
        );
    }
}
```

- [ ] **Step 2: Add the Tauri command**

Append to `cli_tools.rs`:

```rust
/// Symlink the bundled `mur` into a PATH dir. Returns the install path on
/// success. Surfaced to the UI and the tray menu.
#[tauri::command]
pub fn install_cli_tools() -> Result<String, String> {
    let hub_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let src = bundled_mur_path(&hub_exe).ok_or("cannot locate bundled mur")?;
    if !src.exists() {
        return Err(format!("bundled mur not found at {}", src.display()));
    }
    let home = dirs::home_dir().ok_or("no home dir")?;
    let homebrew = Path::new("/opt/homebrew/bin");
    let writable = homebrew.exists()
        && std::fs::metadata(homebrew)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false);
    let dir = install_dir(writable, &home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dst = dir.join("mur");
    let _ = std::fs::remove_file(&dst);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst.display().to_string())
}
```

- [ ] **Step 3: Register module, command, and tray item**

In `mur-hub-gui/src-tauri/src/lib.rs`:
- add `pub mod cli_tools;` to the module list;
- add `cli_tools::install_cli_tools,` to the `tauri::generate_handler![ ... ]` list (after `companion_quiet,`);
- in the tray menu construction (lib.rs:247-249), add an item and handle it:

```rust
            let cli_item = MenuItem::with_id(
                app, "install_cli", "Install Command-Line Tools…", true, None::<&str>,
            )?;
            let menu = Menu::with_items(app, &[&open_item, &cli_item, &quit_item])?;
```

and in the `.on_menu_event(...)` match (lib.rs:254), add:

```rust
                    "install_cli" => {
                        match cli_tools::install_cli_tools() {
                            Ok(p) => { let _ = app.emit("cli-tools-installed", p); }
                            Err(e) => { let _ = app.emit("cli-tools-error", e); }
                        }
                    }
```

- [ ] **Step 4: Run tests + build**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml cli_tools`
Expected: PASS (3 tests).
Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: builds.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/cli_tools.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): Install Command-Line Tools menu item"
```

---

## Phase 6 — Upgrade nudge (spec §16)

### Task 12: Brain-badge + dismiss-remember backend

**Files:**
- Create: `mur-hub-gui/src-tauri/src/brain_badge.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (module + commands)
- Test: in `mur-hub-gui/src-tauri/src/brain_badge.rs` (`#[cfg(test)]`)

Scope note: the nudge's UI (the badge widget and the in-character ceiling prompt) is frontend work in `mur-hub-gui/ui` and is covered in Task 14. This task delivers the backend state the UI binds to: the current model label, and a durable "don't ask again" flag. The "capability ceiling" trigger is a UI/agent policy (when the local model declines or the user asks for more); the backend only records dismissal and exposes the current model.

- [ ] **Step 1: Write the failing test + logic**

Create `mur-hub-gui/src-tauri/src/brain_badge.rs`:

```rust
//! Backend state for the model-upgrade nudge (spec §16): current model label
//! and a durable "don't ask again" flag. No timers, ever.

use std::path::{Path, PathBuf};

const DISMISS_MARKER: &str = ".upgrade_nudge_dismissed";

pub fn dismiss_marker_path(mur_home: &Path) -> PathBuf {
    mur_home.join(DISMISS_MARKER)
}

pub fn is_nudge_dismissed(mur_home: &Path) -> bool {
    dismiss_marker_path(mur_home).exists()
}

pub fn dismiss_nudge(mur_home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(mur_home)?;
    std::fs::write(dismiss_marker_path(mur_home), "")
}

/// Read the seed Mur agent's current model name from its profile, if present.
pub fn current_model_label(mur_home: &Path) -> Option<String> {
    let body = std::fs::read_to_string(mur_home.join("agents/mur/profile.yaml")).ok()?;
    let profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&body).ok()?;
    Some(profile.model.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn dismiss_is_durable() {
        let home = TempDir::new().unwrap();
        assert!(!is_nudge_dismissed(home.path()));
        dismiss_nudge(home.path()).unwrap();
        assert!(is_nudge_dismissed(home.path()));
    }

    #[test]
    fn model_label_reads_from_profile() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("agents/mur")).unwrap();
        std::fs::write(
            home.path().join("agents/mur/profile.yaml"),
            "name: Mur\nmodel:\n  provider: local\n  name: Qwen3.5-2B-MLX-4bit\n",
        )
        .unwrap();
        assert_eq!(
            current_model_label(home.path()).as_deref(),
            Some("Qwen3.5-2B-MLX-4bit")
        );
    }
}
```

- [ ] **Step 2: Add Tauri commands**

Append to `brain_badge.rs`:

```rust
#[tauri::command]
pub fn nudge_status() -> (bool, Option<String>) {
    let home = crate::mur_home_path();
    (is_nudge_dismissed(&home), current_model_label(&home))
}

#[tauri::command]
pub fn nudge_dismiss() -> Result<(), String> {
    dismiss_nudge(&crate::mur_home_path()).map_err(|e| e.to_string())
}
```

- [ ] **Step 3: Register module + commands**

In `lib.rs`: add `pub mod brain_badge;`; add `brain_badge::nudge_status, brain_badge::nudge_dismiss,` to the `generate_handler!` list. (`mur_home_path` is already `fn` in lib.rs:352; make it `pub(crate)` if the module can't see it.)

- [ ] **Step 4: Run tests + build**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml brain_badge`
Expected: PASS (2 tests).
Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: builds.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/brain_badge.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): model-upgrade nudge backend (badge + dismiss-remember)"
```

---

### Task 13: Frontend brain badge + ceiling-triggered prompt

**Files:**
- Modify: `mur-hub-gui/ui/` (existing dashboard/popover React app — locate the main layout component and add the badge).

Scope note: this is UI wiring; verify by running the dev app, not unit tests.

- [ ] **Step 1: Add the passive brain badge**

In the Hub UI's main chrome component (find it under `mur-hub-gui/ui/src/` — the component that renders the popover/dashboard header), call the backend and render a low-key badge:

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

function BrainBadge() {
  const [model, setModel] = useState<string | null>(null);
  useEffect(() => {
    invoke<[boolean, string | null]>("nudge_status").then(([, m]) => setModel(m));
  }, []);
  if (!model) return null;
  return (
    <button
      className="brain-badge"
      title="目前的大腦 — 點此升級成更聰明的模型"
      onClick={() => invoke("open_dashboard").then(() => location.hash = "#/models")}
    >
      🧠 {model}
    </button>
  );
}
```

Render `<BrainBadge />` in the header. Add minimal CSS (`.brain-badge`) consistent with the existing styles (small, muted, non-intrusive).

- [ ] **Step 2: Add the ceiling-triggered prompt (once, dismissable)**

Where the chat surface handles an agent reply, when the agent signals it cannot do something with the local brain (e.g. a reply tagged by the concierge skill, or the user explicitly asks to "be smarter"), show a one-time in-character banner unless dismissed:

```tsx
async function maybeShowUpgradeNudge(showBanner: (text: string) => void) {
  const [dismissed] = await invoke<[boolean, string | null]>("nudge_status");
  if (dismissed) return;
  showBanner("這個我現在的小腦袋有點吃力～要幫我接上更聰明的大腦嗎？");
}
// On the banner's "不用了" action:
async function onDismiss() { await invoke("nudge_dismiss"); }
// On the banner's "好啊" action: navigate to the model wizard (#/models).
```

- [ ] **Step 3: Verify in the dev app**

Run (two terminals, per `mur-hub-gui/README.md`):
```bash
cd mur-hub-gui/ui && npm run dev
cargo tauri dev --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```
Expected: the brain badge shows the current model; triggering the ceiling path shows the banner once; "不用了" prevents it returning after restart.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui
git commit -m "feat(hub): brain badge + once-only upgrade nudge UI"
```

---

## Phase 7 — Release pipeline + docs

### Task 14: Hub build/sign/notarize job in `release.yml`

**Files:**
- Modify: `.github/workflows/release.yml`

Context: the existing macOS signing/notarization for `mur-*.dmg` lives in `release.yml` (around lines 220-290). Reuse the same identity/secrets. Add a new job that runs on a macOS Apple-Silicon runner.

- [ ] **Step 1: Add the Hub job**

Append a job to `.github/workflows/release.yml` (mirror the env/secrets names used by the existing macOS dmg steps — `APPLE_*`, signing identity, `notarytool` keychain profile):

```yaml
  hub-macos:
    name: Build MuR Hub (macOS arm64)
    runs-on: macos-14
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: "20" }
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: aarch64-apple-darwin }
      - name: Build Hub UI
        run: cd mur-hub-gui/ui && npm ci && npm run build
      - name: Build mur + runtime sidecars
        run: |
          cargo build --release --target aarch64-apple-darwin -p mur-core -p mur-agent-runtime
          mkdir -p mur-hub-gui/src-tauri/binaries
          cp target/aarch64-apple-darwin/release/mur \
             mur-hub-gui/src-tauri/binaries/mur-aarch64-apple-darwin
          cp target/aarch64-apple-darwin/release/mur-agent-runtime \
             mur-hub-gui/src-tauri/binaries/mur-agent-runtime-aarch64-apple-darwin
      - name: Build mlx-server sidecar
        run: bash scripts/build-mlx-server.sh aarch64-apple-darwin
      - name: Fetch bundled model
        run: bash scripts/fetch-bundle-model.sh mlx-community/Qwen3.5-2B-MLX-4bit
      - name: Install tauri CLI
        run: cargo install tauri-cli --version "^2" --locked
      - name: Build, sign, bundle .dmg
        env:
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_SIGNING_IDENTITY: ${{ secrets.APPLE_SIGNING_IDENTITY }}
        run: cargo tauri build --manifest-path mur-hub-gui/src-tauri/Cargo.toml --target aarch64-apple-darwin
      - name: Notarize + staple
        env:
          APPLE_ID: ${{ secrets.APPLE_ID }}
          APPLE_PASSWORD: ${{ secrets.APPLE_PASSWORD }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
        run: |
          DMG=$(find mur-hub-gui/src-tauri/target -name "*.dmg" | head -n1)
          xcrun notarytool submit "$DMG" \
            --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
          xcrun stapler staple "$DMG"
          mkdir -p dist && cp "$DMG" "dist/MuR-Hub-aarch64-apple-darwin.dmg"
      - name: Upload to release
        uses: softprops/action-gh-release@v2
        with:
          files: dist/MuR-Hub-aarch64-apple-darwin.dmg
```

(Match the exact secret names already present in `release.yml`; if the existing dmg steps use a keychain-profile approach instead of `--apple-id`, copy that approach verbatim for consistency.)

- [ ] **Step 2: Validate workflow YAML**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml')); print('OK')"`
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: build/sign/notarize MuR Hub .dmg on release"
```

---

### Task 15: README Quick Start entry

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add the Hub download to Quick Start**

In `README.md` Quick Start (around line 47), add above the `curl` one-liner:

```markdown
### Easiest — MuR Hub for macOS (Apple Silicon)

Download **[MuR Hub.dmg](https://github.com/mur-run/mur/releases/latest)**, drag
**MuR Hub** to Applications, and open it. A built-in agent named **Mur** is ready
immediately — offline, no API key. Double-click any `.muragent` a friend sends to
install and run it. (For the CLI, use Hub's *Install Command-Line Tools* menu, or
the one-liner below.)
```

- [ ] **Step 2: Verify the doc renders / link present**

Run: `grep -n "MuR Hub for macOS" README.md`
Expected: the new heading line is found.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add MuR Hub macOS download to Quick Start"
```

---

## Phase 8 — End-to-end verification

### Task 16: Workspace gates + manual smoke test

- [ ] **Step 1: Full workspace test + lint**

Run: `cargo test --workspace`
Expected: all pass.
Run: `cargo clippy --workspace -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 2: Local bundle build (requires Apple Silicon + tooling)**

Run:
```bash
bash scripts/build-mlx-server.sh aarch64-apple-darwin
bash scripts/fetch-bundle-model.sh mlx-community/Qwen3.5-2B-MLX-4bit
cargo build --release --target aarch64-apple-darwin -p mur-core -p mur-agent-runtime
cp target/aarch64-apple-darwin/release/mur mur-hub-gui/src-tauri/binaries/mur-aarch64-apple-darwin
cp target/aarch64-apple-darwin/release/mur-agent-runtime mur-hub-gui/src-tauri/binaries/mur-agent-runtime-aarch64-apple-darwin
cd mur-hub-gui/ui && npm ci && npm run build && cd ../..
cargo tauri build --manifest-path mur-hub-gui/src-tauri/Cargo.toml --target aarch64-apple-darwin
```
Expected: a `.dmg` is produced under `mur-hub-gui/src-tauri/target/.../bundle/dmg/`.

- [ ] **Step 3: Manual smoke test (clean user dir)**

```bash
export MUR_HOME="$(mktemp -d)/.mur"
open "mur-hub-gui/src-tauri/target/aarch64-apple-darwin/release/bundle/macos/MuR Hub.app"
```
Verify:
- Hub launches; the seed **Mur** agent appears and replies in Traditional Chinese, offline.
- `~/.mur/runtime/local_llm.url` exists and points at the live sidecar port.
- The brain badge shows `Qwen3.5-2B-MLX-4bit`.
- *Install Command-Line Tools* puts `mur` on PATH (`which mur`).
- Double-clicking a sample `.muragent` installs and starts that agent.
- Gatekeeper: `spctl -a -t open --context context:primary-signature "<the>.dmg"` passes.

- [ ] **Step 4: Final commit (if any fixups were needed)**

```bash
git add -A && git commit -m "chore: e2e fixups for self-contained Hub install"
```

---

## Self-Review Notes (coverage map)

- Spec §4 (distribution) → Tasks 9, 10, 14.
- Spec §5 (bundled model) → Tasks 2, 10 (fetch), 14.
- Spec §6 (MLX sidecar + provider) → Tasks 1, 3, 5, 6.
- Spec §7 (seed Mur) → Tasks 7, 8.
- Spec §8 (runtime resolution) → Task 4.
- Spec §9 (first-run + `.muragent`) → Task 8 (seed/first-run); `.muragent` open already implemented in `lib.rs` (single-instance/deep-link/RunEvent::Opened) and `import_muragent.rs` — no new work, verified in Task 16.
- Spec §10 (CLI tools menu) → Task 11.
- Spec §11 (release pipeline) → Tasks 14, 15.
- Spec §16 (upgrade nudge) → Tasks 12, 13.
- Spec §17 (watch-together scene) → out of scope here; lives in `2026-06-02-companion-media-skills-design.md` plan (depends on the local endpoint delivered by Tasks 1/3/6).
- Spec §13 (testing) → unit tests in Tasks 1–12; manual E2E in Task 16.
