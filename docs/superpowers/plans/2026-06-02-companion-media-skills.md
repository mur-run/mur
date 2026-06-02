# Companion Media Skills — Implementation Plan (`vlc-control` + `scene-explain`)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let any MuR agent (notably the seed "Mur") watch a movie with the user and explain what's on screen — control VLC (local file or YouTube) and explain the current frame with the local multimodal model, in Traditional Chinese, fully offline.

**Architecture:** All control and frame capture go through **VLC's HTTP interface** (no libVLC bindings). New logic lives in `mur-core` under `cmd/media/`; it is surfaced to agents as MCP tools in `mur-mcp-server` and taught via two bundled skill manifests installed by `mur sync`. Frame explanation calls the local OpenAI-compatible MLX endpoint whose base URL is published by the install plan (`mur_common::local_llm::read_base_url`).

**Tech Stack:** Rust (mur-core, mur-mcp-server), `reqwest` (already a mur-core dep), VLC HTTP interface (`requests/status.xml` + `command=`), OpenAI-compatible vision chat against the local MLX server.

**Dependency:** Requires `mur_common::local_llm` (install plan Task 1) for the local model base URL, and benefits from the running MLX sidecar (install plan Tasks 3/6). The control/snapshot half (Phase 1) has no such dependency and can land independently.

**Key decisions (resolving companion-media spec §8):**
- **Frame capture = VLC HTTP `command=snapshot`** into a per-session `--snapshot-path` temp dir, then read the newest file. No libVLC binding.
- **YouTube = VLC's built-in resolver** via `command=in_play&input=<url>`. `yt-dlp` bundling deferred (note logged if playback fails).
- **MCP surface = 4 tools:** `vlc_open`, `vlc_playback`, `vlc_status`, `scene_explain`.
- **VLC HTTP auth:** Basic auth, empty username + a random per-session password; **port + password generated at runtime**, persisted to `~/.mur/runtime/vlc.json`. No hardcoded port/password.

---

## File Structure

**Create:**
- `mur-core/src/cmd/media/mod.rs` — module decls, `VlcRuntime` config (port/password gen + persist), shared `VlcStatus` type.
- `mur-core/src/cmd/media/vlc.rs` — detection, ensure-running, HTTP control (open/playback/status), XML parse.
- `mur-core/src/cmd/media/scene.rs` — snapshot capture + VLM explain (request build + response parse).
- `mur-core/src/skills/vlc_control.yaml` — `vlc-control` skill manifest.
- `mur-core/src/skills/scene_explain.yaml` — `scene-explain` skill manifest.

**Modify:**
- `mur-core/src/cmd/mod.rs` — add `pub mod media;`.
- `mur-core/src/cmd/sync_cmd.rs:1128` — add the two skills to the `include_str!` install array.
- `mur-mcp-server/src/tools.rs` — register 4 tool defs in `all_tools()` and dispatch in `call_tool()`.

---

## Phase 1 — VLC control core (mur-core, mostly TDD)

### Task 1: `VlcRuntime` config — port/password generation + persistence

**Files:**
- Create: `mur-core/src/cmd/media/mod.rs`
- Modify: `mur-core/src/cmd/mod.rs`
- Test: in `mur-core/src/cmd/media/mod.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test + module**

Create `mur-core/src/cmd/media/mod.rs`:

```rust
//! Media companion: control VLC and explain the current frame with the local
//! multimodal model. All control uses VLC's HTTP interface (no libVLC).

pub mod scene;
pub mod vlc;

use serde::{Deserialize, Serialize};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

/// Per-session VLC HTTP connection details. Generated once and persisted so
/// repeated tool calls reach the same running VLC instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VlcRuntime {
    pub port: u16,
    pub password: String,
    /// Directory VLC writes snapshots to (`--snapshot-path`).
    pub snapshot_dir: PathBuf,
}

/// Reserve a free localhost TCP port.
pub fn pick_free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// 32-hex-char random password from the OS RNG (not a long-term secret, but
/// must not be guessable by other local users).
pub fn gen_password() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).expect("OS RNG");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn runtime_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("vlc.json")
}

/// Load the persisted runtime config, if present and parseable.
pub fn load_runtime(mur_home: &Path) -> Option<VlcRuntime> {
    let body = std::fs::read_to_string(runtime_path(mur_home)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Persist the runtime config atomically.
pub fn save_runtime(mur_home: &Path, rt: &VlcRuntime) -> std::io::Result<()> {
    let path = runtime_path(mur_home);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(rt).unwrap())?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn password_is_32_hex_chars() {
        let p = gen_password();
        assert_eq!(p.len(), 32);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn runtime_roundtrips() {
        let home = TempDir::new().unwrap();
        assert!(load_runtime(home.path()).is_none());
        let rt = VlcRuntime {
            port: 50990,
            password: "abc123".into(),
            snapshot_dir: home.path().join("snaps"),
        };
        save_runtime(home.path(), &rt).unwrap();
        assert_eq!(load_runtime(home.path()), Some(rt));
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-core/src/cmd/mod.rs`, add alongside the other `pub mod` lines:

```rust
pub mod media;
```

- [ ] **Step 3: Ensure `getrandom` is available**

Run: `grep -n "getrandom" mur-core/Cargo.toml`
If absent, add under `[dependencies]`: `getrandom = "0.2"` (or match the workspace version if pinned). `serde_json`, `serde`, `tempfile` are already deps of mur-core.

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-core media::mod::tests`
Expected: PASS (2 tests). (If the test path filter misses, use `cargo test -p mur-core password_is_32_hex_chars runtime_roundtrips`.)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/media/mod.rs mur-core/src/cmd/mod.rs mur-core/Cargo.toml
git commit -m "feat(media): VlcRuntime config (port/password gen + persistence)"
```

---

### Task 2: VLC detection + HTTP URL/command builders + status parse

**Files:**
- Create: `mur-core/src/cmd/media/vlc.rs`
- Test: in `mur-core/src/cmd/media/vlc.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test + pure helpers**

Create `mur-core/src/cmd/media/vlc.rs`:

```rust
//! VLC control via the HTTP interface.

use super::VlcRuntime;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Default macOS VLC binary path; overridable via `MUR_VLC_PATH`.
pub fn detect_vlc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MUR_VLC_PATH") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let candidate = Path::new("/Applications/VLC.app/Contents/MacOS/VLC");
    candidate.exists().then(|| candidate.to_path_buf())
}

/// Parsed subset of VLC's `requests/status.xml`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VlcStatus {
    pub state: String,   // "playing" | "paused" | "stopped"
    pub time: i64,       // seconds elapsed
    pub length: i64,     // seconds total
    pub volume: i64,     // raw VLC volume (256 == 100%)
}

/// Base URL for the VLC HTTP interface.
pub fn status_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/requests/status.xml")
}

/// Build a command URL: `…/status.xml?command=<cmd>[&<extra>]`.
pub fn command_url(port: u16, cmd: &str, extra: &[(&str, &str)]) -> String {
    let mut url = format!("{}?command={}", status_url(port), cmd);
    for (k, v) in extra {
        url.push('&');
        url.push_str(k);
        url.push('=');
        url.push_str(&urlencoding::encode(v));
    }
    url
}

/// Extract the text between `<tag>` and `</tag>` (first occurrence).
fn tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

/// Parse the subset of status.xml we use. Missing fields default sensibly.
pub fn parse_status_xml(xml: &str) -> VlcStatus {
    VlcStatus {
        state: tag(xml, "state").unwrap_or_else(|| "stopped".into()),
        time: tag(xml, "time").and_then(|s| s.parse().ok()).unwrap_or(0),
        length: tag(xml, "length").and_then(|s| s.parse().ok()).unwrap_or(0),
        volume: tag(xml, "volume").and_then(|s| s.parse().ok()).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_respects_env_override() {
        // Point at a path that does not exist → None.
        unsafe { std::env::set_var("MUR_VLC_PATH", "/no/such/vlc") };
        assert_eq!(detect_vlc(), None);
        unsafe { std::env::remove_var("MUR_VLC_PATH") };
    }

    #[test]
    fn command_url_encodes_extra() {
        let u = command_url(8080, "in_play", &[("input", "https://x/y?a=b")]);
        assert!(u.starts_with("http://127.0.0.1:8080/requests/status.xml?command=in_play&input="));
        assert!(u.contains("https%3A%2F%2Fx%2Fy%3Fa%3Db"));
    }

    #[test]
    fn parse_status_extracts_fields() {
        let xml = "<root><volume>256</volume><state>playing</state><time>42</time><length>3600</length></root>";
        let s = parse_status_xml(xml);
        assert_eq!(s.state, "playing");
        assert_eq!(s.time, 42);
        assert_eq!(s.length, 3600);
        assert_eq!(s.volume, 256);
    }
}
```

- [ ] **Step 2: Ensure `urlencoding` is available**

Run: `grep -n "urlencoding" mur-core/Cargo.toml`
If absent, add `urlencoding = "2"` under `[dependencies]`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-core media::vlc::tests`
Expected: PASS (3 tests).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/media/vlc.rs mur-core/Cargo.toml
git commit -m "feat(media): VLC detection, HTTP command builders, status parser"
```

---

### Task 3: Ensure VLC running + HTTP control functions

**Files:**
- Modify: `mur-core/src/cmd/media/vlc.rs`
- Test: build-only (network/process I/O verified manually in Phase 4)

- [ ] **Step 1: Add the runtime accessor + spawn**

Append to `vlc.rs`:

```rust
use super::{gen_password, load_runtime, pick_free_port, save_runtime};

/// Get the persisted runtime or create + persist a fresh one (does not spawn).
fn ensure_runtime(mur_home: &Path) -> Result<VlcRuntime> {
    if let Some(rt) = load_runtime(mur_home) {
        return Ok(rt);
    }
    let rt = VlcRuntime {
        port: pick_free_port().context("pick free port")?,
        password: gen_password(),
        snapshot_dir: mur_home.join("runtime").join("vlc-snapshots"),
    };
    std::fs::create_dir_all(&rt.snapshot_dir).ok();
    save_runtime(mur_home, &rt)?;
    Ok(rt)
}

/// Spawn VLC with the HTTP interface + snapshot path if it is not already
/// answering on the configured port.
async fn ensure_vlc_running(mur_home: &Path, client: &reqwest::Client) -> Result<VlcRuntime> {
    let rt = ensure_runtime(mur_home)?;
    // Probe: if status responds, VLC is up.
    if client
        .get(status_url(rt.port))
        .basic_auth("", Some(&rt.password))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
    {
        return Ok(rt);
    }
    let vlc = detect_vlc().context("VLC not found (install VLC.app)")?;
    std::process::Command::new(vlc)
        .args([
            "--extraintf=http",
            "--http-host=127.0.0.1",
            &format!("--http-port={}", rt.port),
            &format!("--http-password={}", rt.password),
            "--snapshot-format=png",
            &format!("--snapshot-path={}", rt.snapshot_dir.display()),
        ])
        .spawn()
        .context("spawn VLC")?;
    // Give the HTTP iface a moment to come up.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if client
            .get(status_url(rt.port))
            .basic_auth("", Some(&rt.password))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return Ok(rt);
        }
    }
    anyhow::bail!("VLC HTTP interface did not come up on port {}", rt.port)
}

async fn get_status(rt: &VlcRuntime, client: &reqwest::Client) -> Result<VlcStatus> {
    let xml = client
        .get(status_url(rt.port))
        .basic_auth("", Some(&rt.password))
        .send()
        .await?
        .text()
        .await?;
    Ok(parse_status_xml(&xml))
}

async fn send_command(
    rt: &VlcRuntime,
    client: &reqwest::Client,
    cmd: &str,
    extra: &[(&str, &str)],
) -> Result<VlcStatus> {
    let xml = client
        .get(command_url(rt.port, cmd, extra))
        .basic_auth("", Some(&rt.password))
        .send()
        .await?
        .text()
        .await?;
    Ok(parse_status_xml(&xml))
}
```

- [ ] **Step 2: Add the public API used by MCP tools**

Append to `vlc.rs`:

```rust
fn mur_home() -> Result<PathBuf> {
    crate::cmd::resolve_mur_home()
}

/// Open a local file path or a URL (e.g. YouTube) in VLC.
pub async fn open(source: &str) -> Result<VlcStatus> {
    let client = reqwest::Client::new();
    let home = mur_home()?;
    let rt = ensure_vlc_running(&home, &client).await?;
    send_command(&rt, &client, "in_play", &[("input", source)]).await
}

/// Playback control. `action` ∈ {play, pause, toggle, stop, seek, volume}.
/// `value` is seconds (seek) or raw VLC volume (volume).
pub async fn playback(action: &str, value: Option<f64>) -> Result<VlcStatus> {
    let client = reqwest::Client::new();
    let home = mur_home()?;
    let rt = ensure_vlc_running(&home, &client).await?;
    let v = value.unwrap_or(0.0);
    let vs = format!("{}", v as i64);
    match action {
        "play" => send_command(&rt, &client, "pl_forceresume", &[]).await,
        "pause" => send_command(&rt, &client, "pl_forcepause", &[]).await,
        "toggle" => send_command(&rt, &client, "pl_pause", &[]).await,
        "stop" => send_command(&rt, &client, "pl_stop", &[]).await,
        "seek" => send_command(&rt, &client, "seek", &[("val", &vs)]).await,
        "volume" => send_command(&rt, &client, "volume", &[("val", &vs)]).await,
        other => anyhow::bail!("unknown playback action: {other}"),
    }
}

/// Current playback status.
pub async fn status() -> Result<VlcStatus> {
    let client = reqwest::Client::new();
    let home = mur_home()?;
    let rt = ensure_vlc_running(&home, &client).await?;
    get_status(&rt, &client).await
}

/// Internal accessor for scene.rs: ensure running and return the runtime.
pub(super) async fn ensure_for_snapshot(
    client: &reqwest::Client,
) -> Result<VlcRuntime> {
    let home = mur_home()?;
    ensure_vlc_running(&home, client).await
}

pub(super) async fn snapshot_command(
    rt: &VlcRuntime,
    client: &reqwest::Client,
) -> Result<()> {
    let _ = send_command(rt, client, "snapshot", &[]).await?;
    Ok(())
}
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p mur-core`
Expected: builds. (`crate::cmd::resolve_mur_home` already exists — used by `tools.rs:301`.)

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/media/vlc.rs
git commit -m "feat(media): ensure-running + VLC HTTP open/playback/status"
```

---

## Phase 2 — scene-explain (mur-core)

### Task 4: Snapshot capture (newest-file selection)

**Files:**
- Create: `mur-core/src/cmd/media/scene.rs`
- Test: in `mur-core/src/cmd/media/scene.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test + pure helper**

Create `mur-core/src/cmd/media/scene.rs`:

```rust
//! scene-explain: capture the current VLC frame and explain it with the local
//! multimodal model.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Return the most recently modified regular file in `dir`, if any.
pub fn newest_file(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let mtime = entry.metadata().ok()?.modified().ok()?;
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn newest_file_picks_latest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.png"), b"a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("b.png"), b"b").unwrap();
        assert_eq!(
            newest_file(dir.path()).unwrap().file_name().unwrap(),
            "b.png"
        );
    }

    #[test]
    fn newest_file_empty_dir_is_none() {
        let dir = TempDir::new().unwrap();
        assert!(newest_file(dir.path()).is_none());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p mur-core media::scene::tests::newest_file`
Expected: PASS (2 tests).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/media/scene.rs
git commit -m "feat(media): newest-file selection for VLC snapshots"
```

---

### Task 5: VLM request build + response parse

**Files:**
- Modify: `mur-core/src/cmd/media/scene.rs`
- Test: in `mur-core/src/cmd/media/scene.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test + pure builders**

Append to `scene.rs`:

```rust
use serde_json::{Value, json};

/// Default instruction when the caller gives no prompt.
pub const DEFAULT_EXPLAIN_PROMPT: &str =
    "用繁體中文、溫暖簡潔地說明這個畫面正在發生什麼；若有人物或字幕，也一併解讀。";

/// Build an OpenAI-compatible vision chat request body.
pub fn build_request(model: &str, prompt: &str, image_data_url: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": image_data_url } }
            ]
        }],
        "max_tokens": 512
    })
}

/// Encode PNG bytes as a data URL for the image_url field.
pub fn png_data_url(bytes: &[u8]) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:image/png;base64,{b64}")
}

/// Extract assistant text from an OpenAI-compatible chat completion response.
pub fn parse_completion(resp: &Value) -> Option<String> {
    resp.get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn request_has_text_and_image_parts() {
        let body = build_request("Qwen3.5-2B-MLX-4bit", "hi", "data:image/png;base64,AAA");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAA");
    }

    #[test]
    fn data_url_prefixed() {
        assert!(png_data_url(b"\x89PNG").starts_with("data:image/png;base64,"));
    }

    #[test]
    fn parse_extracts_content() {
        let resp = serde_json::json!({
            "choices": [{ "message": { "content": "這是一隻貓" } }]
        });
        assert_eq!(parse_completion(&resp).as_deref(), Some("這是一隻貓"));
    }
}
```

- [ ] **Step 2: Ensure `base64` is available**

Run: `grep -n "^base64" mur-core/Cargo.toml`
If absent, add `base64 = "0.22"` under `[dependencies]`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p mur-core media::scene::request_tests`
Expected: PASS (3 tests).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/media/scene.rs mur-core/Cargo.toml
git commit -m "feat(media): VLM vision request build + response parse"
```

---

### Task 6: `explain()` orchestration

**Files:**
- Modify: `mur-core/src/cmd/media/scene.rs`
- Test: build-only (live VLM verified in Phase 4)

- [ ] **Step 1: Add the orchestrator**

Append to `scene.rs`:

```rust
use mur_common::config::DEFAULT_BUNDLED_MODEL_ID;

/// Resolve the local model endpoint base URL (e.g. http://127.0.0.1:PORT/v1).
fn local_base_url() -> Result<String> {
    let home = crate::cmd::resolve_mur_home()?;
    mur_common::local_llm::read_base_url(&home)
        .context("local model endpoint not available (is MuR Hub running?)")
}

/// Capture the current VLC frame and explain it with the local multimodal model.
pub async fn explain(prompt: Option<&str>) -> Result<String> {
    let client = reqwest::Client::new();

    // 1. Ensure VLC is up and take a snapshot.
    let rt = super::vlc::ensure_for_snapshot(&client).await?;
    super::vlc::snapshot_command(&rt, &client).await?;

    // 2. Read the newest snapshot file (retry briefly for the file to land).
    let mut img_path = None;
    for _ in 0..10 {
        if let Some(p) = newest_file(&rt.snapshot_dir) {
            img_path = Some(p);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    let img_path = img_path.context("no snapshot produced by VLC")?;
    let bytes = std::fs::read(&img_path).context("read snapshot")?;

    // 3. Call the local OpenAI-compatible vision endpoint.
    let base = local_base_url()?;
    let body = build_request(
        DEFAULT_BUNDLED_MODEL_ID,
        prompt.unwrap_or(DEFAULT_EXPLAIN_PROMPT),
        &png_data_url(&bytes),
    );
    let resp: serde_json::Value = client
        .post(format!("{}/chat/completions", base.trim_end_matches('/')))
        .json(&body)
        .send()
        .await
        .context("call local VLM")?
        .json()
        .await
        .context("parse VLM response")?;

    parse_completion(&resp).context("VLM returned no content")
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p mur-core`
Expected: builds. (Requires `mur_common::local_llm` from the install plan Task 1; if that module is not yet merged, land install-plan Task 1 first or stub it.)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/media/scene.rs
git commit -m "feat(media): scene explain — snapshot then local VLM narration"
```

---

## Phase 3 — MCP tools

### Task 7: Register `vlc_*` + `scene_explain` MCP tools

**Files:**
- Modify: `mur-mcp-server/src/tools.rs` (`all_tools()` and `call_tool()`)

- [ ] **Step 1: Add tool definitions**

In `mur-mcp-server/src/tools.rs`, inside `all_tools()` (before the closing `]` of the `vec![`), add four `Tool` entries:

```rust
        // ── media tools ──
        Tool {
            name: "vlc_open".into(),
            description: "Open a local video file path or a URL (e.g. a YouTube link) in VLC and start playing. Returns playback status.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(vec![
                    ("source".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Local file path or video URL (YouTube supported)".into(),
                        default: None,
                    }),
                ]),
                required: Some(vec!["source".into()]),
            },
        },
        Tool {
            name: "vlc_playback".into(),
            description: "Control VLC playback. action ∈ play|pause|toggle|stop|seek|volume. For seek, value=seconds; for volume, value=0-512 (256=100%).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(vec![
                    ("action".into(), ToolParam {
                        param_type: "string".into(),
                        description: "play|pause|toggle|stop|seek|volume".into(),
                        default: None,
                    }),
                    ("value".into(), ToolParam {
                        param_type: "number".into(),
                        description: "Seconds (seek) or volume level (volume)".into(),
                        default: None,
                    }),
                ]),
                required: Some(vec!["action".into()]),
            },
        },
        Tool {
            name: "vlc_status".into(),
            description: "Get current VLC playback status (state, time, length, volume). Use before narrating so the explanation matches the current frame.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "scene_explain".into(),
            description: "Capture the current VLC frame and explain what is on screen using the local multimodal model (offline, private). Optionally pass a specific question.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(vec![
                    ("prompt".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Optional question about the frame; defaults to a general description".into(),
                        default: None,
                    }),
                ]),
                required: None,
            },
        },
```

- [ ] **Step 2: Add dispatch arms**

In `call_tool()`, before the final `_ => Err(format!("Unknown tool: {}", name))` arm, add:

```rust
        "vlc_open" => {
            let source = arguments
                .get("source")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'source' (string)".to_string())?;
            let status = mur_core::cmd::media::vlc::open(source)
                .await
                .map_err(|e| format!("vlc_open failed: {}", e))?;
            Ok(serde_json::to_value(status).unwrap_or(Value::Null))
        }

        "vlc_playback" => {
            let action = arguments
                .get("action")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'action' (string)".to_string())?;
            let value = arguments.get("value").and_then(|v| v.as_f64());
            let status = mur_core::cmd::media::vlc::playback(action, value)
                .await
                .map_err(|e| format!("vlc_playback failed: {}", e))?;
            Ok(serde_json::to_value(status).unwrap_or(Value::Null))
        }

        "vlc_status" => {
            let status = mur_core::cmd::media::vlc::status()
                .await
                .map_err(|e| format!("vlc_status failed: {}", e))?;
            Ok(serde_json::to_value(status).unwrap_or(Value::Null))
        }

        "scene_explain" => {
            let prompt = arguments.get("prompt").and_then(|v| v.as_str());
            let text = mur_core::cmd::media::scene::explain(prompt)
                .await
                .map_err(|e| format!("scene_explain failed: {}", e))?;
            Ok(json!({ "explanation": text }))
        }
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p mur-mcp-server`
Expected: builds.

- [ ] **Step 4: Sanity-check tools/list count**

Add a test to `mur-mcp-server/src/tools.rs` (`#[cfg(test)]`), then run it:

```rust
#[cfg(test)]
mod media_tool_tests {
    use super::*;
    #[test]
    fn media_tools_registered() {
        let names: Vec<_> = all_tools().into_iter().map(|t| t.name).collect();
        for n in ["vlc_open", "vlc_playback", "vlc_status", "scene_explain"] {
            assert!(names.contains(&n.to_string()), "missing {n}");
        }
    }
}
```

Run: `cargo test -p mur-mcp-server media_tools_registered`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-mcp-server/src/tools.rs
git commit -m "feat(mcp): vlc_open/playback/status + scene_explain tools"
```

---

## Phase 4 — Skills + verification

### Task 8: Bundle the two skill manifests

**Files:**
- Create: `mur-core/src/skills/vlc_control.yaml`
- Create: `mur-core/src/skills/scene_explain.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs:1128` (the `include_str!` install array)

- [ ] **Step 1: Write `vlc_control.yaml`**

Create `mur-core/src/skills/vlc_control.yaml` (mirror the field shape of `mur_project_search.yaml`):

```yaml
name: vlc-control
version: 0.1.0
publisher: human:mur
description: "Watch and control video in VLC — open a local file or a YouTube link, play/pause/seek/volume, and check status."
category: media
hosts: [all]
content:
  abstract: |
    To watch a movie with the user, drive VLC via the MCP tools: vlc_open
    (local path or YouTube URL), vlc_playback (play/pause/toggle/stop/seek/volume),
    and vlc_status. Check vlc_status before narrating so commentary matches the
    current frame.
  context: |
    # vlc-control — watch together

    Use these tools when the user wants to watch or control a video:
    - vlc_open(source): start a local file or a URL (YouTube supported).
    - vlc_playback(action, value?): play | pause | toggle | stop | seek | volume.
    - vlc_status(): current state/time/length/volume.

    Be warm and concise. Pause before explaining a frame. DRM-protected streaming
    services (Netflix etc.) cannot be captured — decline gracefully if asked.
tags: [mur, media, vlc, video, builtin]
triggers:
  - type: keyword
    pattern: "(watch (a )?(movie|video)|play .{0,30}(in vlc|on vlc)|看(電影|影片)|一起看|youtube)"
  - type: manual
priority: normal
```

- [ ] **Step 2: Write `scene_explain.yaml`**

Create `mur-core/src/skills/scene_explain.yaml`:

```yaml
name: scene-explain
version: 0.1.0
publisher: human:mur
description: "Explain what is on screen in the currently playing video, using the local multimodal model (offline, private)."
category: media
hosts: [all]
content:
  abstract: |
    When the user asks what's happening on screen, call scene_explain (optionally
    with a specific question). It captures the current VLC frame and explains it
    with the local model — no cloud, nothing uploaded.
  context: |
    # scene-explain — narrate the current frame

    Call scene_explain(prompt?) to describe or interpret the current video frame.
    Pause playback first (vlc_playback action=pause) so the frame is stable.
    Answer in the user's language; default to warm Traditional Chinese.
    Everything runs locally — emphasize privacy if the user asks.
tags: [mur, media, vision, video, builtin]
triggers:
  - type: keyword
    pattern: "(what('| i)s (happening|on screen|this scene)|explain (this|the) (scene|frame)|這(一)?幕|畫面.{0,6}(說明|解說)|他(剛)?說(的|了)什麼)"
  - type: manual
priority: normal
```

- [ ] **Step 3: Register in the install array**

In `mur-core/src/cmd/sync_cmd.rs`, the `skills` array (starts at line 1128) ends with the `mur-session-remove` entry near line 1151. Add two entries before the closing `]`:

```rust
        (
            "vlc-control",
            include_str!("../skills/vlc_control.yaml"),
        ),
        (
            "scene-explain",
            include_str!("../skills/scene_explain.yaml"),
        ),
```

- [ ] **Step 4: Verify it builds (include_str! resolves)**

Run: `cargo build -p mur-core`
Expected: builds (fails loudly if either YAML path is wrong).

- [ ] **Step 5: Verify manifests parse as skills**

If a skill-manifest loader/validator exists (e.g. `mur skill doctor` or a parse fn), run it; otherwise add a YAML well-formedness check:

Run: `python3 -c "import yaml; yaml.safe_load(open('mur-core/src/skills/vlc_control.yaml')); yaml.safe_load(open('mur-core/src/skills/scene_explain.yaml')); print('OK')"`
Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/skills/vlc_control.yaml mur-core/src/skills/scene_explain.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): bundle vlc-control + scene-explain manifests"
```

---

### Task 9: Workspace gates + manual end-to-end

**Files:** none (verification)

- [ ] **Step 1: Workspace test + lint**

Run: `cargo test --workspace`
Expected: all pass.
Run: `cargo clippy --workspace -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 2: Install skills locally**

Run: `cargo run -- sync`
Then: `ls ~/.mur/skills/vlc-control ~/.mur/skills/scene-explain`
Expected: each contains `skill.yaml` (and rendered `SKILL.md`).

- [ ] **Step 3: Manual VLC control (requires VLC.app + a running MLX endpoint)**

Ensure MuR Hub is running (so `~/.mur/runtime/local_llm.url` exists), then drive the MCP tools (via the MCP server or a quick harness):
- `vlc_open` a YouTube URL → VLC opens and plays.
- `vlc_playback action=pause` → playback pauses; `vlc_status` shows `state=paused`.
- `scene_explain prompt="這一幕在演什麼？"` → returns a Traditional-Chinese description of the paused frame, produced offline.

Verify privacy: with network disabled after the page loads, `scene_explain` still returns a description (model is local).

- [ ] **Step 4: Final commit (if fixups were needed)**

```bash
git add -A && git commit -m "chore(media): e2e fixups for watch-together skills"
```

---

## Self-Review Notes (coverage map)

- Spec §3 (architecture: skills teach, MCP tools act) → Tasks 7, 8.
- Spec §4 (`vlc-control`: detect / open file+YouTube / play-pause-seek-volume-status) → Tasks 2, 3, 7, 8.
- Spec §5 (`scene-explain`: capture + local VLM, zh-TW) → Tasks 4, 5, 6, 7, 8.
- Spec §6 (orchestration hooks: detection, idle auto-pause) → `detect_vlc()` (Task 2) exposes detection; idle auto-pause reuses the existing C6 idle triggers calling `vlc_playback action=pause` (no new code here; wired by the companion layer).
- Spec §7 (out of scope: DRM, auto-narration, yt-dlp default) → honored; DRM/decline guidance is in the skill manifests (Task 8).
- Spec §8 (open questions) → resolved in "Key decisions": HTTP snapshot, VLC built-in YouTube resolver, 4-tool surface, runtime port/password file.
- Spec §9 (testing) → unit tests Tasks 1,2,4,5,7; manual E2E Task 9.
- Spec §10 (affected components) → `mur-core/src/cmd/media/` (Tasks 1-6), `mur-mcp-server` (Task 7), skills + local endpoint reuse (Task 8, Task 6).

**Cross-plan dependency:** Task 6 (`scene::explain`) needs `mur_common::local_llm::read_base_url` from the install plan (Task 1). Land that first, or stub the reader. Phase 1 (VLC control) is independent and can ship on its own.
