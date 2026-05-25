# M4b — Peer Transfer (Wire) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace M4a's direct local-store read with a real A2A round-trip. `mur skill install agent://<agent-name>/<skill-name>` dials the source agent's `skills/get` over its Unix socket (if running) or boots the runtime in stdio mode briefly (if not). The install pipeline, trust model, transfer-chain semantics, and profile registration stay exactly as M4a defines them.

**What M4b changes vs M4a:**
- `install_from_agent` no longer calls `read_from_dir(global_skill_dir(home, name))` directly. It issues a JSON-RPC `skills/get` request to the source agent and parses the response.
- A shared `mur-core/src/a2a_dial.rs` module replaces the duplicated dial logic that currently lives in `mur-core/src/cmd/agent/comm.rs`. `cmd/agent/comm.rs` (`cmd_send`, `cmd_card`) and the new skill install path both consume it.
- An integration test boots a real runtime and verifies the socket path end-to-end.

**What M4b does NOT change:**
- The handler implementation (`mur-agent-runtime/src/protocol/methods/skills.rs`) is already correct from M4a.
- `cmd_install`'s URL parsing, trust resolution, `register_in_profile`, and the M4a integration test all stay as-is.
- Spec deltas (`agent://X`-without-slash form rejected, single-shot transfer instead of two-stage, partial `provenance` block) remain in effect.

**Deployment assumption (still single-host, single-user):** Source and target agents share one `MUR_HOME`. The wire dial uses the local Unix socket from `<MUR_HOME>/agents/<name>/running.lock`. Cross-host TCP+Noise transport exists in the runtime but is deferred to a later milestone — the dial helper rejects TCP-only endpoints for now and returns a clear error so users aren't silently downgraded.

**Tech Stack:** Rust 2024, existing `serde_json`, `tokio` (already used by the runtime, not by the install path — install stays sync). Zero new dependencies.

---

## File Structure

**Create:**
- `mur-core/src/a2a_dial.rs` — shared dial helper (`dial_method`, ephemeral spawn fallback)
- `mur-core/tests/skill_install_agent_wire.rs` — wire-level integration test

**Modify:**
- `mur-core/src/lib.rs` (or equivalent module root) — `pub mod a2a_dial;`
- `mur-core/src/cmd/agent/comm.rs` — delegate to `a2a_dial`, remove the duplicated `dial_rpc` + `ephemeral_card_via_runtime`
- `mur-core/src/cmd/skill_install.rs` — replace local read in `install_from_agent` with a `a2a_dial::dial_method` call

---

### Task 1 — Extract `a2a_dial` shared module

**Files:** `mur-core/src/a2a_dial.rs` (new), `mur-core/src/lib.rs`, `mur-core/src/cmd/agent/comm.rs`.

The current `dial_rpc` and `ephemeral_card_via_runtime` in `cmd/agent/comm.rs` are tightly coupled to the `agent/card` use case (hardcoded request bytes in the ephemeral path, no caller-supplied id, no method-agnostic response parsing). Extract a clean helper before changing behavior so the M4a `mur agent card` / `mur agent send` flows can be regression-tested against the extracted code unchanged.

- [ ] **Step 1: Create `a2a_dial.rs`**

```rust
//! Shared A2A dial helper — issues a JSON-RPC request to a local agent,
//! either via its running Unix socket or by spawning the runtime
//! ephemerally in stdio mode.
//!
//! Consumed by:
//!   - `cmd/agent/comm.rs` for `mur agent card` / `mur agent send`
//!   - `cmd/skill_install.rs` for `mur skill install agent://...`

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use mur_common::LockFile;
use serde_json::{Value, json};

use crate::cmd::agent::resolve_runtime_target;

/// Strategy for reaching the target agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialMode {
    /// Use the running agent's socket if available, otherwise spawn an
    /// ephemeral runtime in stdio mode. Default for CLI use.
    Auto,
    /// Require the target agent to be running. Fail otherwise. Used by
    /// flows that must not pay the cold-start cost (or where ephemeral
    /// spawn would mask a misconfiguration).
    RequireRunning,
    /// Always spawn an ephemeral runtime. Useful for tests and for
    /// pulling skills from agents that the user explicitly does not want
    /// to keep resident.
    ForceEphemeral,
}

/// Dial the named agent and return the `result` field of the JSON-RPC
/// response. Errors carry the agent name and method for diagnosability.
///
/// `request_id` must match `request["id"]`; the helper enforces this so
/// callers can't accidentally race their own requests.
pub fn dial_method(
    home: &Path,
    agent_name: &str,
    method: &str,
    params: Value,
    mode: DialMode,
) -> Result<Value> {
    let request_id = json!(1);
    let request = json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": params,
    });

    let lock_path = home.join("agents").join(agent_name).join("running.lock");
    let is_running = lock_path.exists();

    match (mode, is_running) {
        (DialMode::RequireRunning, false) => bail!(
            "agent '{agent_name}' is not running (no {})",
            lock_path.display()
        ),
        (DialMode::ForceEphemeral, _) => {
            dial_ephemeral(home, agent_name, &request, &request_id)
        }
        (_, true) => dial_socket(&lock_path, agent_name, &request, &request_id),
        (_, false) => dial_ephemeral(home, agent_name, &request, &request_id),
    }
}

fn dial_socket(
    lock_path: &Path,
    agent_name: &str,
    request: &Value,
    request_id: &Value,
) -> Result<Value> {
    let bytes = fs::read(lock_path).with_context(|| format!("read {}", lock_path.display()))?;
    let lock: LockFile = serde_json::from_slice(&bytes).context("parse running.lock")?;
    let sock = lock.transports.unix_socket.ok_or_else(|| {
        anyhow!(
            "agent '{agent_name}' has no unix-socket transport (TCP-only transports are not yet supported by the install path)"
        )
    })?;

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(&sock)
            .with_context(|| format!("connect {sock}"))?;
        let line = format!("{}\n", serde_json::to_string(request)?);
        stream.write_all(line.as_bytes())?;
        let reader = BufReader::new(stream.try_clone()?);
        for line in reader.lines() {
            let line = line.context("read response line")?;
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id") == Some(request_id) {
                if let Some(err) = v.get("error") {
                    bail!("agent '{agent_name}' returned error: {err}");
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        bail!("EOF before matching response from '{agent_name}'");
    }
    #[cfg(not(unix))]
    {
        let _ = sock;
        bail!("unix socket transport is only supported on unix hosts")
    }
}

fn dial_ephemeral(
    home: &Path,
    agent_name: &str,
    request: &Value,
    request_id: &Value,
) -> Result<Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let runtime = resolve_runtime_target();
    if !runtime.is_absolute() && !runtime.exists() {
        bail!(
            "agent '{agent_name}' not running and runtime binary not found at {} (set MUR_AGENT_RUNTIME_BIN)",
            runtime.display()
        );
    }

    let mut child = std::process::Command::new(&runtime)
        .env("MUR_HOME", home)
        .args(["--profile", agent_name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", runtime.display()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin on spawned runtime"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no stdout on spawned runtime"))?;

    let req_line = format!("{}\n", serde_json::to_string(request)?);
    stdin
        .write_all(req_line.as_bytes())
        .context("write to runtime stdin")?;
    drop(stdin);

    let reader = BufReader::new(stdout);
    let mut found: Option<Value> = None;
    let mut last_err: Option<Value> = None;
    for line in reader.lines() {
        let line = line.context("read runtime stdout")?;
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("id") == Some(request_id) {
            if let Some(err) = v.get("error") {
                last_err = Some(err.clone());
                break;
            }
            found = Some(v.get("result").cloned().unwrap_or(Value::Null));
            break;
        }
    }

    // Best-effort SIGTERM. The ephemeral runtime will also exit when its
    // stdin closes, but we don't want to wait indefinitely.
    #[cfg(unix)]
    {
        let pid = child.id();
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    let _ = child.wait();

    if let Some(err) = last_err {
        bail!("agent '{agent_name}' returned error: {err}");
    }
    found.ok_or_else(|| anyhow!("ephemeral runtime did not produce a response for '{agent_name}'"))
}
```

- [ ] **Step 2: Register the module**

In `mur-core/src/lib.rs`, add:

```rust
pub mod a2a_dial;
```

(Confirm the existing module list — append in alphabetical / structural order to match siblings.)

- [ ] **Step 3: Refactor `cmd/agent/comm.rs` to use the helper**

Replace the body of `dial_rpc` and remove `ephemeral_card_via_runtime`:

```rust
use crate::a2a_dial::{DialMode, dial_method};

pub fn cmd_send(name: &str, message_json: &str) -> Result<()> {
    let msg: serde_json::Value =
        serde_json::from_str(message_json).context("parse --message JSON")?;
    let home = resolve_mur_home()?;
    let params = serde_json::json!({"message": msg});
    // `message/send` to an ephemeral runtime is meaningless — the task
    // would die with the process. Require the agent be running.
    let result = dial_method(&home, name, "message/send", params, DialMode::RequireRunning)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn cmd_card(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let result = dial_method(&home, name, "agent/card", serde_json::Value::Null, DialMode::Auto)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
```

The two helper functions (`dial_rpc`, `ephemeral_card_via_runtime`) and their imports can be deleted from `comm.rs` after this change.

> Sanity-check: `mur agent send` to a non-running agent previously errored with `agent '...' is not running (no ...)`. The new `DialMode::RequireRunning` path produces the same message via `dial_method`. Verify by reading the existing tests for `cmd_send` (if any) and adjusting expectations only if the exact substring changed.

- [ ] **Step 4: Add unit tests for the helper**

In `a2a_dial.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn require_running_fails_without_lock() {
        let home = tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/nobody")).unwrap();
        let err = dial_method(
            home.path(),
            "nobody",
            "agent/card",
            Value::Null,
            DialMode::RequireRunning,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not running"));
    }

    #[test]
    fn auto_mode_falls_through_to_ephemeral_when_no_lock() {
        // Without a runtime binary on PATH the ephemeral spawn fails
        // with a recognizable error — that's what we assert, since this
        // is a pure unit test.
        let home = tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/nobody")).unwrap();
        unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", "/does/not/exist") };
        let err = dial_method(
            home.path(),
            "nobody",
            "agent/card",
            Value::Null,
            DialMode::Auto,
        )
        .unwrap_err();
        unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
        let msg = err.to_string();
        assert!(
            msg.contains("runtime binary not found") || msg.contains("spawn"),
            "unexpected error: {msg}"
        );
    }
}
```

> **Env-var caveat:** Rust 2024 marks `std::env::set_var` `unsafe`. Run this file's tests with `--test-threads=1`, or stash the env var locally and skip the test when it's already set, to avoid cross-test interference.

- [ ] **Step 5: Build + test + commit**

```bash
cargo build -p mur-core
cargo test -p mur-core a2a_dial
cargo test -p mur-core cmd::agent::comm
git add mur-core/src/a2a_dial.rs mur-core/src/lib.rs mur-core/src/cmd/agent/comm.rs
git commit -m "refactor(a2a): extract dial helper used by skill install and agent comm"
```

---

### Task 2 — Rewire `install_from_agent` to dial the socket

**Files:** `mur-core/src/cmd/skill_install.rs`.

Replace the M4a direct-read with a real `skills/get` round-trip. The downstream pipeline (trust → scan → chain append → write → profile register) is unchanged.

- [ ] **Step 1: Replace the local-read block**

In `install_from_agent`, current Step 2 (lines 122-127 of `skill_install.rs`):

```rust
// 2. Pull — read the skill directly from the shared local store.
let source_dir = global_skill_dir(home, skill_name);
let mut manifest: SkillManifest = read_from_dir(&source_dir)
    .map_err(|e| anyhow!("pull from agent://{agent_name}/{skill_name} failed: {e}"))?;
let received_hash =
    content_hash_for_trust(&manifest).map_err(|e| anyhow!("hash source manifest: {e}"))?;
```

Replace with:

```rust
// 2. Pull — JSON-RPC `skills/get` to the source agent.
use crate::a2a_dial::{DialMode, dial_method};
use mur_common::skill::parse_canonical;

let response = dial_method(
    home,
    agent_name,
    "skills/get",
    serde_json::json!({"skill": skill_name}),
    DialMode::Auto,
)
.with_context(|| format!("pull agent://{agent_name}/{skill_name}"))?;

let manifest_yaml = response
    .get("manifest")
    .and_then(|v| v.as_str())
    .ok_or_else(|| {
        anyhow!("agent://{agent_name}/{skill_name}: response missing 'manifest' field")
    })?;
let mut manifest: SkillManifest = parse_canonical(manifest_yaml)
    .with_context(|| format!("parse manifest from agent://{agent_name}/{skill_name}"))?;

let advertised_hash = response
    .get("content_sha256")
    .and_then(|v| v.as_str())
    .ok_or_else(|| {
        anyhow!("agent://{agent_name}/{skill_name}: response missing 'content_sha256'")
    })?;
let received_hash = content_hash_for_trust(&manifest)
    .map_err(|e| anyhow!("hash received manifest: {e}"))?;

// The sender's advertised hash must match what we just computed.
// Otherwise the payload was tampered with in transit or the sender is
// buggy — either way, refuse the install rather than poisoning the
// trust store.
if advertised_hash != received_hash {
    bail!(
        "agent://{agent_name}/{skill_name}: hash mismatch \
         (sender advertised {advertised_hash}, computed {received_hash}) — install blocked"
    );
}
```

Top-of-file: remove `read_from_dir` from the `mur_common::skill::{...}` import if no longer used (the registry install path still uses `global_skill_dir`/`scan_skill`/`write_to_dir`, so those stay).

- [ ] **Step 2: Update the "discover" preamble**

The current Step 1 of `install_from_agent` checks `home.join("agents").join(agent_name)` exists. Keep this — it provides a clean error before we attempt the dial, and prevents the ephemeral fallback from creating profile-less garbage processes.

But weaken the error message — it's no longer about M4a's single-MUR_HOME constraint, just about whether we know the agent:

```rust
// 1. Discover — confirm the named agent is registered on this host.
let agent_dir = home.join("agents").join(agent_name);
if !agent_dir.exists() {
    bail!(
        "agent '{agent_name}' not found at {} — cannot dial",
        agent_dir.display()
    );
}
```

- [ ] **Step 3: Confirm tests still pass against the new path**

The existing M4a integration test (`mur-core/tests/skill_install_agent_e2e.rs`) wrote a skill to alice's local store and expected `install_from_agent` to read it via `read_from_dir`. With M4b, that test would need either:

(a) a real running runtime for alice (heavy — see Task 3), or
(b) the ephemeral spawn path (also heavy, requires the runtime binary built).

Move the existing M4a tests into a new file:

```bash
git mv mur-core/tests/skill_install_agent_e2e.rs \
       mur-core/tests/skill_install_agent_local_legacy.rs
```

Mark them `#[ignore]` with a comment pointing at the wire test added in Task 3:

```rust
//! Legacy M4a integration tests — these exercised the direct-read
//! shortcut that M4b replaces with a wire dial. Kept for archaeology
//! and as a quick smoke test when temporarily reverting to local-read
//! for debugging. The current behavior is covered by
//! `skill_install_agent_wire.rs`.

#![allow(dead_code)]

// ... existing test bodies, each annotated with #[ignore = "M4a-only"] ...
```

> If you'd rather delete the legacy file outright, do so in this commit and skip the rename. The trade-off: archaeology vs. file churn. Prefer rename for the first M4b cycle; clean up in a later sweep.

- [ ] **Step 4: Build + commit**

```bash
cargo build -p mur-core
git add mur-core/src/cmd/skill_install.rs \
        mur-core/tests/skill_install_agent_local_legacy.rs
git rm mur-core/tests/skill_install_agent_e2e.rs  # if you renamed
git commit -m "feat(skill): install agent:// pulls via A2A wire (replaces M4a local-read)"
```

---

### Task 3 — Wire-level integration test

**Files:** `mur-core/tests/skill_install_agent_wire.rs` (new).

This test must boot a real `mur-agent-runtime` and dial its socket. Two options:

**Option A (preferred):** Boot the runtime as a subprocess via the `mur-agent-runtime` binary built by `cargo test`. The `MUR_AGENT_RUNTIME_BIN` env var picks the binary; tests set it to `env!("CARGO_BIN_EXE_mur-agent-runtime")` (Cargo populates this for binary deps).

**Option B:** In-process via `mur_agent_runtime::supervisor::run(...)` on a `tokio::spawn`. Lower overhead but couples the test crate to the runtime crate, which currently isn't a `dev-dependency` of `mur-core`. Adding the dep is fine but pulls a lot of compile time into `mur-core` tests.

Use **Option A** — matches how real users invoke the install flow, and the runtime binary is already built when `cargo test --workspace` runs.

- [ ] **Step 1: Add the runtime as a `dev-dependency` so its binary is available**

In `mur-core/Cargo.toml` under `[dev-dependencies]`:

```toml
[dev-dependencies]
# ... existing dev-deps ...
mur-agent-runtime = { path = "../mur-agent-runtime", artifact = "bin" }
```

> The `artifact = "bin"` feature is a stable cargo feature on edition 2024. If the workspace pins an older cargo, fall back to building the runtime explicitly in `build.rs` and reading its path from an env var. Confirm with `cargo --version` (must be ≥ 1.71 for `bindeps`; check `Cargo.toml` for `cargo-features = ["bindeps"]` if needed).

- [ ] **Step 2: Write the wire test**

```rust
//! Wire-level integration test for `mur skill install agent://...`.
//!
//! Boots a real `mur-agent-runtime` for the source agent, dials its
//! Unix socket for `skills/get`, and verifies that the install
//! pipeline applies trust, appends the transfer chain, writes the
//! skill, and registers it on the calling agent's profile.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use mur_common::agent::AgentProfile;
use mur_common::skill::{
    TrustLevel, content_hash_for_trust, global_skill_dir, parse_canonical, read_from_dir,
    write_to_dir,
};
use mur_common::trust::skills::SkillTrustStore;
use mur_core::cmd::skill_install::cmd_install;
use tempfile::TempDir;

/// Path to the runtime binary, populated by cargo's `bindeps` feature.
const RUNTIME_BIN: &str = env!("CARGO_BIN_EXE_mur-agent-runtime");

fn write_profile(home: &std::path::Path, name: &str, unix_sock: Option<&str>) -> std::path::PathBuf {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut profile = AgentProfile {
        name: name.to_string(),
        ..AgentProfile::default_for_tests()
    };
    profile.transport.stdio = false;
    profile.transport.socket.enabled = unix_sock.is_some();
    if let Some(sock) = unix_sock {
        profile.transport.socket.bind = format!("unix://{sock}");
    }
    let yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let path = dir.join("profile.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

struct RuntimeGuard {
    child: Child,
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let _ = self.child.wait();
    }
}

fn boot_runtime(home: &std::path::Path, agent: &str) -> RuntimeGuard {
    let child = Command::new(RUNTIME_BIN)
        .env("MUR_HOME", home)
        .args(["--profile", agent])
        .spawn()
        .expect("spawn runtime");
    // Wait for running.lock to appear (the supervisor writes it after
    // binding the socket).
    let lock = home.join("agents").join(agent).join("running.lock");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if lock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(lock.exists(), "runtime did not write running.lock within 5s");
    RuntimeGuard { child }
}

#[test]
fn wire_install_pulls_via_real_socket() {
    let home = TempDir::new().unwrap();
    let sock_path = home.path().join("alice.sock").to_str().unwrap().to_string();

    // Source agent "alice".
    write_profile(home.path(), "alice", Some(&sock_path));
    let manifest = parse_canonical(
        r#"
name: find-prices
version: 1.0.0
publisher: human:alice
description: Find product prices
category: workflow
content:
  abstract: Searches product prices.
  context: "Full procedure."
"#,
    )
    .unwrap();
    write_to_dir(&global_skill_dir(home.path(), "find-prices"), &manifest).unwrap();

    // Target agent "bob".
    let bob_profile = write_profile(home.path(), "bob", None);

    // Boot alice; her runtime serves `skills/get` on the unix socket.
    let _alice = boot_runtime(home.path(), "alice");

    // Install. SAFETY: see env-var note at top.
    unsafe { std::env::set_var("MUR_AGENT_NAME", "bob") };
    unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", RUNTIME_BIN) };
    let result = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://alice/find-prices",
    );
    unsafe { std::env::remove_var("MUR_AGENT_NAME") };
    unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
    result.unwrap();

    // 1. Skill file is on disk with transfer_chain appended.
    let installed = read_from_dir(&global_skill_dir(home.path(), "find-prices")).unwrap();
    assert_eq!(installed.transfer_chain, vec!["agent://alice"]);

    // 2. Trust entry is Sandboxed (no registry cache).
    let trust = SkillTrustStore::load(home.path()).unwrap();
    let key = content_hash_for_trust(&installed).unwrap();
    let entry = trust.lookup(&key).expect("trust entry exists");
    assert!(matches!(entry.level, TrustLevel::Sandboxed));

    // 3. Bob's profile carries the SkillCardEntry.
    let bob_yaml = std::fs::read_to_string(&bob_profile).unwrap();
    let bob: AgentProfile = serde_yaml_ng::from_str(&bob_yaml).unwrap();
    assert_eq!(bob.installed_skills.len(), 1);
    assert_eq!(bob.installed_skills[0].publisher, "human:alice");
}

#[test]
fn wire_install_uses_ephemeral_when_source_offline() {
    let home = TempDir::new().unwrap();

    // Source agent "carol" — profile present but NOT running. The
    // install path must spawn the runtime ephemerally to serve
    // skills/get over stdio.
    write_profile(home.path(), "carol", None);
    let manifest = parse_canonical(
        r#"
name: offline-skill
version: 1.0.0
publisher: human:carol
description: d
category: context
content:
  abstract: a
  context: b
"#,
    )
    .unwrap();
    write_to_dir(&global_skill_dir(home.path(), "offline-skill"), &manifest).unwrap();

    write_profile(home.path(), "dave", None);
    unsafe { std::env::set_var("MUR_AGENT_NAME", "dave") };
    unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", RUNTIME_BIN) };
    let result = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://carol/offline-skill",
    );
    unsafe { std::env::remove_var("MUR_AGENT_NAME") };
    unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
    result.unwrap();

    let installed = read_from_dir(&global_skill_dir(home.path(), "offline-skill")).unwrap();
    assert_eq!(installed.transfer_chain, vec!["agent://carol"]);
}

#[test]
fn wire_install_propagates_handler_error_for_missing_skill() {
    let home = TempDir::new().unwrap();
    let sock_path = home.path().join("eve.sock").to_str().unwrap().to_string();
    write_profile(home.path(), "eve", Some(&sock_path));
    let _eve = boot_runtime(home.path(), "eve");

    unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", RUNTIME_BIN) };
    let err = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://eve/no-such-skill",
    )
    .unwrap_err();
    unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
    let msg = err.to_string();
    assert!(
        msg.contains("not found") || msg.contains("Internal"),
        "unexpected error: {msg}"
    );
}
```

- [ ] **Step 3: Run + commit**

```bash
cargo test -p mur-core --test skill_install_agent_wire -- --test-threads=1
git add mur-core/tests/skill_install_agent_wire.rs mur-core/Cargo.toml
git commit -m "test(skill): wire integration — agent:// install over real socket + ephemeral fallback"
```

> **CI consideration:** these tests are heavier (each one spawns the runtime). If workspace CI already runs `cargo test --workspace`, no change needed — the test count increases by 3. If CI has a fast-test job that skips integration tests, this file matches the existing `tests/*` convention so it should be included automatically. Verify by reading `.github/workflows/*.yml` or the workspace's CI config before merging.

---

### Task 4 — Documentation + observability hooks

**Files:** `docs/architecture/runtime-overview.md` (if present), `mur-core/src/cmd/skill_install.rs` (tracing spans).

- [ ] **Step 1: Add `tracing` spans on the dial path**

In `a2a_dial.rs::dial_method`, wrap the entry:

```rust
let span = tracing::info_span!("a2a.dial", agent = %agent_name, method = %method).entered();
tracing::debug!(?mode, "dialing");
```

And in `install_from_agent`'s wire pull block:

```rust
tracing::info!(skill = %skill_name, source = %agent_name, "pulling skill via A2A");
```

These align with the existing `tracing` usage in the codebase (see `RUST_LOG=debug` note in `CLAUDE.md`). No new `tracing` crate features needed.

- [ ] **Step 2: Update the architecture doc snippet (only if present)**

```bash
grep -l "M4a\|peer transfer\|skill install" docs/architecture/*.md 2>/dev/null
```

If `runtime-overview.md` mentions M4a as "direct local read," replace with one paragraph: "Peer install dials the source agent's `skills/get` over its Unix socket; if not running, the runtime is spawned in stdio mode for the single request. Cross-host TCP is deferred." If no such mention exists, skip this step — don't create documentation the codebase doesn't already maintain.

- [ ] **Step 3: Commit (if anything changed)**

```bash
git add -p  # selective
git commit -m "feat(skill): tracing spans on agent:// pull path"
```

---

## Out of scope — deferred to M4c

The spec's §7 lists three pieces that M4b deliberately doesn't ship:

1. **`skills/offer` push handler** (§7.2). Adds a new A2A method where the source agent proactively offers a skill to a peer, plus a CLI (`mur skill offer <peer> <skill>`) and a consent prompt on the receiving side. Substantial UX surface — deserves its own milestone. M4b's pull flow already satisfies the primary peer-transfer use case.
2. **Two-stage L1+L2 → L3 transfer** (§7.1 step 3-4). The `?layer=` parameter would let the requester pull just the abstract first, decide whether to proceed, and only then pull the full body. For programmatic `mur skill install` the user has already committed to the install; for LLM-driven discovery this becomes valuable but that's M7 territory (cross-agent evolution).
3. **TCP+Noise transport for cross-host install**. The runtime already supports `tcp+noise` endpoints (see `LockTransports.tcp`), and the dial helper could read them — but trusted-peer enumeration, address resolution, and noise handshake reuse are non-trivial. The current `dial_socket` explicitly errors on TCP-only endpoints so users get a clear "not yet" rather than a silent failure.

If any of these become blocking before M4c is scheduled, slot them into M4b as Task 5/6 — each is independent of the other two.

---

## Self-Review

**Spec coverage delta vs M4a:**

| Spec § | Requirement | M4a status | M4b task |
|---|---|---|---|
| §7.1 step 2 | `B -> A: GET /skills/{name}` | direct read | T2 (wire dial) |
| §7.3 | `/skills/{name}` endpoint | handler exists | now reachable T2/T3 |
| §7.1 step 4 | `?layer=full` two-stage | n/a | **deferred to M4c** |
| §7.2 | Push offer | n/a | **deferred to M4c** |

**Compile-blocker scan:**
- `dial_method` is sync (no `async fn`) — matches `cmd_install`'s sync interface.
- The new `a2a_dial` module uses only types already in the workspace (`anyhow`, `mur_common::LockFile`, `serde_json`, `libc` on unix).
- `cmd/agent/comm.rs` keeps its public surface (`cmd_send`, `cmd_card`) — no caller changes.
- `bindeps` (`artifact = "bin"`) requires modern cargo; T3 Step 1 calls this out with a fallback.

**Behavior regression scan:**
- `mur agent card` against a non-running agent: M4a does ephemeral, M4b does the same via `DialMode::Auto`. Same observable behavior.
- `mur agent send` against a non-running agent: M4a errors with "not running"; M4b errors the same way via `DialMode::RequireRunning`. Verify exact substring matches existing user expectations / tests.
- `mur skill install agent://X/Y` with the source agent running: M4a reads disk, M4b dials socket. Same skill content arrives. Trust + chain + profile-register identical.
- `mur skill install agent://X/Y` with the source agent NOT running: M4a worked (read disk), M4b ephemeral-spawns the source agent. Latency goes from ~ms to seconds. **Document this** in the install help text.

**Test coverage:**
- T3 covers: running source, offline source (ephemeral fallback), missing skill (error propagation through dispatcher → wire → install).
- Not covered: TCP transport (deferred), concurrent installs (single-host, single-user assumption), source agent crash mid-stream (acceptable for v1 — install errors, no orphan trust entries because trust write happens after the full response).

**Atomic-write guarantee preserved:**  `register_in_profile` still uses temp+rename. The new wire path only changes how the manifest gets into memory; the on-disk steps are byte-identical to M4a.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m4b.md`.

Suggested branch: `feat/skill-ecosystem-m4b`, branched from the current `feat/skill-ecosystem-m4a` tip (commit `4c68d42`). The M4a integration test rename in T2 Step 3 will conflict with `main` if `feat/skill-ecosystem-m4a` lands first — rebase before pushing.

Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration. T3's wire test will need the runtime binary built, so the subagent for T3 should run `cargo build -p mur-agent-runtime` first.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints. Faster if you'll stay attentive; slower if T3's bindeps setup needs trial and error.

Which approach?
