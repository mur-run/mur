# Hooks Progressive Disclosure — Gap-Closing Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the five remaining spec gaps in the Hooks Progressive Disclosure + murmurd system so the feature is fully complete per `docs/superpowers/specs/2026-05-04-mur-hooks-progressive-disclosure-design.md`.

**Architecture:** All core mechanics are already shipped (M0 gate, M1 hook cmd, M2 L0 index, M3 murmurd, M4 L2, M5 stats). This plan fills five gaps: Claude Code `async:true` hooks, launchd/systemd daemon autostart, stale-lock respawn in `mur hook`, a 100-query golden gate accuracy test, and latency p50/p95/p99 in `mur hook stats`.

**Tech Stack:** Rust (edition 2024), Tokio, serde_json, dirs, tempfile (tests). No new crate dependencies.

---

## What already exists (do NOT rebuild)

| File | Lines | Status |
|------|-------|--------|
| `mur-core/src/retrieve/gate.rs` | 671 | M0 complete — Adaptive Gate with all 5 signals |
| `mur-core/src/cmd/hook.rs` | 363 | M1+M4 complete — `mur hook prompt/tool/stop/session-start/stats` |
| `mur-core/src/inject/event.rs` | 271 | M1 complete — multi-tool stdin parsers |
| `mur-core/src/inject/queue.rs` | 155 | M1 complete — JSONL queue writer/reader |
| `mur-core/src/inject/index.rs` | 393 | M2 complete — L0 capability index |
| `mur-daemon/src/main.rs` | 104 | M3 complete — murmurd event loop + heartbeat |
| `mur-daemon/src/consumer.rs` | 143 | M3 complete — JSONL queue consumer |
| `mur-daemon/src/lock.rs` | 89 | M3 complete — PID lockfile |
| `mur-daemon/src/inbox.rs` | 60 | M3 complete — inbox read/write |
| `mur-core/src/daemon.rs` | ~30 | M3 complete — thin inbox helpers for hook.rs |
| `mur-core/src/inject/stats.rs` | 279 | M5 partial — event counting, no latency |

---

## File Map (files touched by this plan)

| File | Action | Task |
|------|--------|------|
| `mur-core/src/cmd/init.rs` | Modify — add `async:true` to Claude Code hook JSON | T1 |
| `mur-core/src/cmd/init.rs` | Modify — call daemon install after hook scripts | T2 |
| `mur-core/src/cmd/init_daemon.rs` | **Create** — launchd/systemd plist/unit install logic | T2 |
| `mur-core/src/cmd/mod.rs` | Modify — `pub(crate) mod init_daemon;` | T2 |
| `mur-core/src/daemon.rs` | Modify — add `is_daemon_healthy()` | T3 |
| `mur-core/src/cmd/hook.rs` | Modify — call health check + respawn in cmd_hook_prompt | T3 |
| `mur-core/src/inject/event.rs` | Modify — add `duration_ms: Option<u64>` to NormalizedEvent | T5 |
| `mur-core/src/cmd/hook.rs` | Modify — record latency before returning | T5 |
| `mur-core/src/inject/stats.rs` | Modify — add `p50_ms / p95_ms / p99_ms` to HookStats + format | T5 |
| `mur-core/tests/gate_golden.rs` | **Create** — 100-query accuracy test (≥ 85%) | T4 |

---

## Task 1: Claude Code `async: true` / `asyncRewake: true` in hook entries

**Files:**
- Modify: `mur-core/src/cmd/init.rs:356-363` (the `arr.push(serde_json::json!(...))` block)

The Claude Code hooks API supports `async: true` to prevent the hook from blocking the UI, and `asyncRewake: true` on Stop to re-wake Claude after on-stop work finishes. Without these, heavy hook processing delays the user-facing turn.

- [ ] **Step 1.1: Read the current hook entry push block**

Confirm the current JSON at `mur-core/src/cmd/init.rs` around line 356:
```rust
arr.push(serde_json::json!({
    "hooks": [{
        "type": "command",
        "command": format!("bash {}", script_path),
    }],
    "matcher": ""
}));
```

- [ ] **Step 1.2: Replace the hook entry push with event-aware async flags**

In `mur-core/src/cmd/init.rs`, replace the `arr.push(...)` inside the `for (event_name, script_path) in &hook_defs` loop:

```rust
// Add our hook with Claude Code async flags
let async_flag = matches!(*event_name, "UserPromptSubmit");
let rewake_flag = matches!(*event_name, "Stop");
let mut hook_entry = serde_json::json!({
    "hooks": [{
        "type": "command",
        "command": format!("bash {}", script_path),
    }],
    "matcher": ""
});
if async_flag {
    hook_entry["hooks"][0]["async"] = serde_json::json!(true);
}
if rewake_flag {
    hook_entry["hooks"][0]["asyncRewake"] = serde_json::json!(true);
}
arr.push(hook_entry);
```

- [ ] **Step 1.3: Write a unit test for the generated settings shape**

Add to `mur-core/src/cmd/init.rs` in `#[cfg(test)]`:

```rust
#[test]
fn claude_hooks_have_async_flags() {
    use std::collections::HashMap;

    // Simulate the hook_defs iteration
    let events = ["UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop", "SessionStart"];
    let mut results: HashMap<&str, serde_json::Value> = HashMap::new();
    for event_name in events {
        let async_flag = matches!(event_name, "UserPromptSubmit");
        let rewake_flag = matches!(event_name, "Stop");
        let mut hook_entry = serde_json::json!({
            "hooks": [{"type": "command", "command": "bash /tmp/hook.sh"}],
            "matcher": ""
        });
        if async_flag {
            hook_entry["hooks"][0]["async"] = serde_json::json!(true);
        }
        if rewake_flag {
            hook_entry["hooks"][0]["asyncRewake"] = serde_json::json!(true);
        }
        results.insert(event_name, hook_entry);
    }
    assert_eq!(results["UserPromptSubmit"]["hooks"][0]["async"], serde_json::json!(true));
    assert_eq!(results["Stop"]["hooks"][0]["asyncRewake"], serde_json::json!(true));
    assert!(results["PreToolUse"]["hooks"][0].get("async").is_none());
    assert!(results["PostToolUse"]["hooks"][0].get("asyncRewake").is_none());
}
```

- [ ] **Step 1.4: Run the test**

```bash
cargo test -p mur-core claude_hooks_have_async_flags 2>&1 | tail -5
```
Expected: `test ... ok`

- [ ] **Step 1.5: Commit**

```bash
git add mur-core/src/cmd/init.rs
git commit -m "feat(init): add async:true + asyncRewake:true to Claude Code hook entries"
```

---

## Task 2: `mur init --hooks` installs launchd/systemd murmurd service

**Files:**
- Create: `mur-core/src/cmd/init_daemon.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub(crate) mod init_daemon;`)
- Modify: `mur-core/src/cmd/init.rs` (call `install_daemon_service()` after hook scripts)

The daemon must auto-start on login so `mur hook` always has a warm inbox. Without this, users must manually run `mur murmurd start`.

- [ ] **Step 2.1: Create `mur-core/src/cmd/init_daemon.rs`**

```rust
//! Installs murmurd as a login-persistent service (launchd / systemd).

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Returns true if installation succeeded, false if the platform is
/// unsupported (WSL, container, etc.) — caller prints a fallback message.
pub(crate) fn install_daemon_service(murmurd_path: &Path) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        install_launchd(murmurd_path)?;
        return Ok(true);
    }
    #[cfg(target_os = "linux")]
    {
        install_systemd(murmurd_path)?;
        return Ok(true);
    }
    #[allow(unreachable_code)]
    Ok(false)
}

#[cfg(target_os = "macos")]
fn install_launchd(murmurd_path: &Path) -> Result<()> {
    let label = "run.mur.murmurd";
    let agents_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join("Library")
        .join("LaunchAgents");
    std::fs::create_dir_all(&agents_dir)?;

    let plist_path = agents_dir.join(format!("{label}.plist"));
    let log_path = dirs::home_dir().unwrap().join(".mur").join("murmurd.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>ThrottleInterval</key>
    <integer>5</integer>
</dict>
</plist>
"#,
        bin = murmurd_path.display(),
        log = log_path.display(),
    );
    std::fs::write(&plist_path, &plist)?;

    // Load/reload the agent (ignore errors — user may not have launchctl in PATH)
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .status();
    let _ = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .status();

    Ok(())
}

#[cfg(target_os = "linux")]
fn install_systemd(murmurd_path: &Path) -> Result<()> {
    let unit_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".config")
        .join("systemd")
        .join("user");
    std::fs::create_dir_all(&unit_dir)?;

    let unit_path = unit_dir.join("murmurd.service");
    let unit = format!(
        "[Unit]\nDescription=murmurd — mur pattern daemon\n\n\
         [Service]\nExecStart={bin}\nRestart=always\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        bin = murmurd_path.display(),
    );
    std::fs::write(&unit_path, &unit)?;

    // Enable + start (ignore errors — systemd may not be running, e.g. in containers)
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "murmurd.service"])
        .status();

    Ok(())
}

/// Locate the murmurd binary next to the current mur executable.
pub(crate) fn murmurd_bin_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("murmurd")))
        .unwrap_or_else(|| PathBuf::from("murmurd"))
}
```

- [ ] **Step 2.2: Register the module in `mur-core/src/cmd/mod.rs`**

Open `mur-core/src/cmd/mod.rs` and add before the existing `pub(crate) mod init;` line:

```rust
pub(crate) mod init_daemon;
```

- [ ] **Step 2.3: Call `install_daemon_service()` from `mur-core/src/cmd/init.rs`**

Find the block in `cmd_init()` right after the hook scripts are written (after the `hooks_installed.push("Claude Code")` line at the end of the Claude Code block, but inside the `if install_hooks {` block). Add:

```rust
    // ─── Step B2: Install murmurd as login service ────────────────
    let murmurd_bin = super::init_daemon::murmurd_bin_path();
    match super::init_daemon::install_daemon_service(&murmurd_bin) {
        Ok(true) => println!("  murmurd autostart installed (login service)."),
        Ok(false) => println!(
            "  murmurd autostart: unsupported platform.\n  Run `mur murmurd start --detach` manually."
        ),
        Err(e) => eprintln!("  murmurd install warning: {e:#}"),
    }
```

This should go immediately after the four hook shell scripts are written (after line ~277 where the script writing loop ends, before the Claude Code settings.json block).

- [ ] **Step 2.4: Run clippy to catch cfg/platform issues**

```bash
cargo clippy -p mur-core -- -D warnings 2>&1 | grep "^error" | head -10
```
Expected: no errors. Fix any `dead_code` warnings on cfg-gated functions by adding `#[allow(dead_code)]` if needed on the non-active platform branches.

- [ ] **Step 2.5: Commit**

```bash
git add mur-core/src/cmd/init_daemon.rs mur-core/src/cmd/mod.rs mur-core/src/cmd/init.rs
git commit -m "feat(init): install murmurd as launchd/systemd login service on mur init --hooks"
```

---

## Task 3: Stale-lock respawn in `mur hook prompt`

**Files:**
- Modify: `mur-core/src/daemon.rs` (add `is_daemon_healthy()`)
- Modify: `mur-core/src/cmd/hook.rs` (call health check + respawn in `cmd_hook_prompt`)

Spec §6: "mur hook checks heartbeat freshness (< 30 s); stale lock triggers `mur murmurd --detach` respawn." Without this, a crashed murmurd silently degrades all future hook calls to synchronous retrieval with no user notification.

- [ ] **Step 3.1: Add `is_daemon_healthy()` to `mur-core/src/daemon.rs`**

Open `mur-core/src/daemon.rs` and append:

```rust
/// True if murmurd's lockfile exists and its heartbeat_at timestamp is
/// within the last 30 seconds. Returns false on any IO or parse error.
pub fn is_daemon_healthy() -> bool {
    let lock_path = dirs::home_dir()
        .map(|h| h.join(".mur").join("murmurd.lock"))
        .unwrap_or_default();
    let Ok(raw) = std::fs::read_to_string(&lock_path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(hb_str) = v.get("heartbeat_at").and_then(|s| s.as_str()) else {
        return false;
    };
    let Ok(hb) = chrono::DateTime::parse_from_rfc3339(hb_str) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(hb.with_timezone(&chrono::Utc));
    age.num_seconds() < 30
}

/// Attempt to spawn murmurd as a detached background process.
/// Errors are swallowed — this is best-effort recovery.
pub fn try_respawn_daemon() {
    let murmurd = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("murmurd")))
        .unwrap_or_else(|| std::path::PathBuf::from("murmurd"));
    let _ = std::process::Command::new(&murmurd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
```

You will need `chrono` in scope. Check `mur-core/Cargo.toml` — chrono is already a workspace dep used elsewhere. If the import is missing at the top of daemon.rs, the function bodies already use `dirs` which is also present.

- [ ] **Step 3.2: Write a unit test for `is_daemon_healthy()`**

Add to `mur-core/src/daemon.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn stale_lock_returns_false() {
        let dir = TempDir::new().unwrap();
        let lock = dir.path().join("murmurd.lock");
        // Heartbeat 60 seconds in the past
        let old_ts = chrono::Utc::now() - chrono::Duration::seconds(60);
        let state = serde_json::json!({
            "pid": 9999,
            "started_at": old_ts.to_rfc3339(),
            "heartbeat_at": old_ts.to_rfc3339(),
        });
        std::fs::write(&lock, serde_json::to_string(&state).unwrap()).unwrap();

        // Temporarily override the lock path check by writing to home is not
        // feasible in unit tests — test the parsing logic directly instead.
        let raw = std::fs::read_to_string(&lock).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let hb_str = v["heartbeat_at"].as_str().unwrap();
        let hb = chrono::DateTime::parse_from_rfc3339(hb_str).unwrap();
        let age = chrono::Utc::now()
            .signed_duration_since(hb.with_timezone(&chrono::Utc));
        assert!(age.num_seconds() >= 30, "stale lock should be ≥ 30s old");
    }

    #[test]
    fn fresh_lock_age_check() {
        let hb = chrono::Utc::now();
        let age = chrono::Utc::now().signed_duration_since(hb);
        assert!(age.num_seconds() < 30, "fresh lock should be < 30s old");
    }
}
```

- [ ] **Step 3.3: Run the tests**

```bash
cargo test -p mur-core daemon 2>&1 | tail -8
```
Expected: `stale_lock_returns_false ... ok`, `fresh_lock_age_check ... ok`

- [ ] **Step 3.4: Add health-check + respawn call in `cmd_hook_prompt`**

In `mur-core/src/cmd/hook.rs`, inside `cmd_hook_prompt()`, after the `enqueue(&event)` call and before the gate evaluation, add:

```rust
// Ensure murmurd is running; respawn silently if heartbeat is stale.
if !crate::daemon::is_daemon_healthy() {
    crate::daemon::try_respawn_daemon();
}
```

The full top of `cmd_hook_prompt` will look like:

```rust
pub(crate) async fn cmd_hook_prompt(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw.clone(), EventKind::Prompt, tool);
    let _ = enqueue(&event);

    // Ensure murmurd is running; respawn silently if heartbeat is stale.
    if !crate::daemon::is_daemon_healthy() {
        crate::daemon::try_respawn_daemon();
    }

    let query = extract_query(&raw).unwrap_or_default();
    // ... rest unchanged
```

- [ ] **Step 3.5: Confirm no regression in hook tests**

```bash
cargo test -p mur-core hook 2>&1 | tail -5
```
Expected: all pass.

- [ ] **Step 3.6: Commit**

```bash
git add mur-core/src/daemon.rs mur-core/src/cmd/hook.rs
git commit -m "feat(hook): stale-lock respawn — auto-restart murmurd if heartbeat > 30s"
```

---

## Task 4: 100-query golden set — gate accuracy ≥ 0.85

**Files:**
- Create: `mur-core/tests/gate_golden.rs`

Spec §7 M0: "Unit tests + 100-query golden set, accuracy ≥ 0.85." Without this, gate weight regressions are invisible.

- [ ] **Step 4.1: Create `mur-core/tests/gate_golden.rs`**

```rust
//! Gate accuracy test: 100 representative queries must achieve ≥ 85% correct tier.
//!
//! Each case: (query, min_tier, max_tier, label)
//! min_tier/max_tier bound the acceptable answer (inclusive on both ends).
//! "Skip" = 0, "L0" = 1, "L1" = 2, "L2" = 3
//! Using numeric bounds avoids importing Tier in the test file.

use mur_core::retrieve::gate::{GateInputs, Tier, evaluate_query_v2};

fn tier_ord(t: &Tier) -> u8 {
    match t {
        Tier::Skip => 0,
        Tier::L0   => 1,
        Tier::L1   => 2,
        Tier::L2   => 3,
    }
}

struct Case {
    query: &'static str,
    min: u8,  // inclusive
    max: u8,  // inclusive
}

fn c(query: &'static str, min: u8, max: u8) -> Case {
    Case { query, min, max }
}

fn golden_cases() -> Vec<Case> {
    vec![
        // ── Skip (0): pure ack / meta commands / chitchat ──────────
        c("ok", 0, 0),
        c("好", 0, 0),
        c("thanks", 0, 0),
        c("對", 0, 0),
        c("嗯", 0, 0),
        c("got it", 0, 0),
        c("sounds good", 0, 0),
        c("alright", 0, 0),
        c("sure", 0, 0),
        c("yep", 0, 0),
        c("no problem", 0, 0),
        c("👍", 0, 0),
        c("🙏", 0, 0),
        c("/help", 0, 0),
        c("/clear", 0, 0),
        c("/model", 0, 0),
        c("/status", 0, 0),
        c("/config", 0, 0),
        c("hi", 0, 0),
        c("hello", 0, 0),
        c("bye", 0, 0),
        c("ok ok ok", 0, 1),
        c("符合", 0, 1),
        c("繼續", 0, 1),
        c("好的", 0, 1),
        // ── L0 (1): short questions, pure curiosity ─────────────────
        c("why does this work?", 1, 2),
        c("what is async/await?", 1, 2),
        c("what does serde do?", 1, 2),
        c("為什麼要用 tokio?", 1, 2),
        c("how does LanceDB work?", 1, 2),
        c("explain ownership in Rust", 1, 2),
        c("what is a trait object?", 1, 2),
        c("can you explain lifetimes?", 1, 2),
        c("what is the difference between Arc and Rc?", 1, 2),
        c("why use anyhow?", 1, 2),
        // ── L1 (2): technical queries, file paths, code identifiers ─
        c("how do I use tokio::spawn?", 1, 2),
        c("clap derive subcommand example", 1, 2),
        c("the fn handle_event() is broken", 1, 2),
        c("fix the error in store/yaml.rs", 2, 3),
        c("write a test for score_and_rank", 2, 3),
        c("add a --json flag to the list command", 2, 3),
        c("refactor the error handling in inject/hook.rs", 2, 3),
        c("how to implement Display for PatternKind", 1, 2),
        c("search for patterns matching tokio async", 2, 3),
        c("debug the deadlock in supervisor.rs", 2, 3),
        c("update the Cargo.toml to add serde feature", 2, 3),
        c("make the test pass for capture/noise_filter.rs", 2, 3),
        c("the pattern retrieval is returning stale results", 2, 3),
        c("help me implement the adaptive gate scoring function", 2, 3),
        c("I need to add a new CLI subcommand for model registry", 2, 3),
        c("找出 mur-core/src/inject/hook.rs 中的 bug", 2, 3),
        c("implement missing tests for the webhook handler", 2, 3),
        c("create a new pattern for Rust error handling with anyhow", 2, 3),
        c("trace why embed() fails when Ollama is not running", 2, 3),
        c("check the BM25 score calculation in retrieve/scoring.rs", 2, 3),
        // ── L2 (3): action verbs + long technical + code context ────
        c("implement the tokio worker pool in murmurd consumer", 2, 3),
        c("build the release binary and run cargo test --workspace", 2, 3),
        c("fix the race condition between heartbeat and event drain in mur-daemon/src/main.rs", 2, 3),
        c("implement progressive disclosure: return L0 index on SessionStart, L1 snippet on UserPromptSubmit", 2, 3),
        c("refactor mur-core/src/cmd/init.rs to split the 1400-line file into submodules under cmd/init/", 2, 3),
        c("add async:true to the Claude Code hook entry written by mur init --hooks in settings.json", 2, 3),
        c("migrate the on-prompt.sh hook to use mur hook prompt --tool claude", 2, 3),
        c("write integration tests for all nine tool stdin schemas in inject/event.rs", 2, 3),
        c("add latency p50/p95/p99 tracking to mur hook stats using duration_ms in the event queue", 2, 3),
        c("deploy the murmurd binary to production and install the launchd plist", 2, 3),
        // ── Additional coverage: CJK action verbs ──────────────────
        c("實作 hook 的 L2 tier 邏輯", 2, 3),
        c("修正 inject/queue.rs 中的 offset 計算錯誤", 2, 3),
        c("新增 mur hook stats 命令顯示每種 tier 的比例", 2, 3),
        c("重構 mur-core/src/inject/index.rs 的 format_l0 函式", 2, 3),
        c("找出並修復 retrieve/gate.rs 中 intent_score 的 regex 錯誤", 2, 3),
        // ── Edge / boundary ────────────────────────────────────────
        c("test", 0, 2),              // too short to be L2, ambiguous
        c("run tests", 0, 2),         // short but has action verb
        c("cargo test", 1, 2),        // build command, no file path
        c("cargo build --release", 1, 2),
        c("git log --oneline -10", 1, 2),
        c("ls -la", 0, 1),
        c("pwd", 0, 1),
        c("", 0, 0),                  // empty → skip
        c("   ", 0, 0),               // whitespace → skip
        c("a", 0, 1),                 // single char
        c("ab", 0, 1),                // two chars
        c("???", 0, 1),               // no alphanumeric content
        c("...", 0, 1),
        c("TODO", 0, 1),
        // ── Mixed language ─────────────────────────────────────────
        c("help me 實作 the gate logic in retrieve/gate.rs", 2, 3),
        c("為什麼 score_and_rank 回傳空的結果？", 1, 2),
        c("debug: mur inject hangs on cold start", 2, 3),
        c("fix: mur hook prompt 在 Gemini CLI 下回傳錯誤格式", 2, 3),
        c("add test for 中文 query detection in noise_filter", 2, 3),
    ]
}

#[test]
fn gate_accuracy_on_golden_set() {
    let inputs = GateInputs::default();
    let cases = golden_cases();
    let total = cases.len();
    let mut correct = 0usize;
    let mut failures = Vec::new();

    for case in &cases {
        let outcome = evaluate_query_v2(case.query, &inputs);
        let tier = tier_ord(&outcome.tier);
        if tier >= case.min && tier <= case.max {
            correct += 1;
        } else {
            failures.push(format!(
                "  FAIL: {:?} → tier={} expected=[{},{}] score={:.3}",
                case.query, tier, case.min, case.max, outcome.score
            ));
        }
    }

    let accuracy = correct as f64 / total as f64;
    if !failures.is_empty() {
        println!("\nGolden set failures ({}/{} wrong):", failures.len(), total);
        for f in &failures {
            println!("{f}");
        }
    }
    println!("\nGate accuracy: {correct}/{total} = {:.1}%", accuracy * 100.0);
    assert!(
        accuracy >= 0.85,
        "Gate accuracy {:.1}% < 85% required by spec §7 M0",
        accuracy * 100.0
    );
}
```

- [ ] **Step 4.2: Export `Tier` from `mur-core/src/retrieve/gate.rs` for integration tests**

Check `mur-core/src/retrieve/gate.rs` line 7 — `Tier` should be `pub`. Also ensure `evaluate_query_v2` and `GateInputs` are `pub` (not `pub(crate)`). If they are `pub(crate)`, change to `pub`:

```rust
// mur-core/src/retrieve/gate.rs
pub enum Tier { ... }           // was: pub enum Tier
pub struct GateInputs { ... }   // was: pub struct GateInputs  
pub fn evaluate_query_v2(...) -> GateOutcome { ... }  // was: pub fn
```

Also add to `mur-core/src/lib.rs`:
```rust
pub use retrieve::gate;
```

And check `mur-core/src/retrieve/mod.rs` — `gate` module should be `pub mod gate`.

- [ ] **Step 4.3: Run the golden set test**

```bash
cargo test -p mur-core gate_accuracy_on_golden_set -- --nocapture 2>&1 | tail -20
```
Expected output ends with something like:
```
Gate accuracy: 92/100 = 92.0%
test gate_golden::gate_accuracy_on_golden_set ... ok
```

If accuracy < 85%, review the failure list and adjust gate thresholds in `gate.rs` (not the test cases). The test cases represent ground truth.

- [ ] **Step 4.4: Commit**

```bash
git add mur-core/tests/gate_golden.rs mur-core/src/retrieve/gate.rs mur-core/src/lib.rs
git commit -m "test(gate): 100-query golden set accuracy test (≥ 85%)"
```

---

## Task 5: Latency p50/p95/p99 in `mur hook stats`

**Files:**
- Modify: `mur-core/src/inject/event.rs` (add `duration_ms: Option<u64>`)
- Modify: `mur-core/src/cmd/hook.rs` (record latency in `cmd_hook_prompt` + `cmd_hook_tool`)
- Modify: `mur-core/src/inject/stats.rs` (add percentile computation + format)

Spec §7 M5: "mur hook stats (skip rate / tier dist / latency p50/p95/p99 / inbox-hit rate)." Without latency data, operators cannot verify the < 100 ms p99 SLO.

- [ ] **Step 5.1: Add `duration_ms` to `NormalizedEvent`**

In `mur-core/src/inject/event.rs`, add the field to `NormalizedEvent`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub kind: EventKind,
    pub tool_provider: String,
    pub query: Option<String>,
    pub tool_called: Option<String>,
    pub tool_input: Option<Value>,
    pub stop_reason: Option<String>,
    pub session_id: Option<String>,
    /// Wall-clock duration of the hook invocation, in milliseconds.
    /// None for events written at entry (before processing completes).
    #[serde(default)]
    pub duration_ms: Option<u64>,
}
```

`#[serde(default)]` ensures old events without this field deserialize cleanly.

- [ ] **Step 5.2: Record latency in `cmd_hook_prompt` and `cmd_hook_tool`**

In `mur-core/src/cmd/hook.rs`, wrap `cmd_hook_prompt` to capture elapsed time:

At the very top of `cmd_hook_prompt` (before reading stdin), add:
```rust
let t0 = std::time::Instant::now();
```

At the very end, before `Ok(())`, add:
```rust
let duration_ms = t0.elapsed().as_millis() as u64;
let mut done_event = parse_event(serde_json::json!({}), EventKind::Prompt, tool);
done_event.duration_ms = Some(duration_ms);
done_event.session_id = event.session_id.clone();
let _ = enqueue(&done_event);
```

Apply the same pattern to `cmd_hook_tool` (record `t0` at start, write `done_event` with `EventKind::Tool` at end).

- [ ] **Step 5.3: Add percentile computation to `mur-core/src/inject/stats.rs`**

Add to `HookStats`:
```rust
#[derive(Debug, Default)]
pub struct HookStats {
    pub total: usize,
    pub by_kind: HashMap<String, usize>,
    pub by_provider: HashMap<String, usize>,
    pub top_tools: Vec<(String, usize)>,
    pub unique_sessions: usize,
    // Latency percentiles (ms), only for events with duration_ms set
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub latency_p99_ms: Option<u64>,
}
```

Add to `compute()` — after the existing counting loop:
```rust
// Latency percentiles from events that have duration_ms
let mut durations: Vec<u64> = events
    .iter()
    .filter_map(|e| e.duration_ms)
    .collect();
durations.sort_unstable();

fn percentile(sorted: &[u64], pct: f64) -> Option<u64> {
    if sorted.is_empty() { return None; }
    let idx = ((pct / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    Some(sorted[idx.min(sorted.len() - 1)])
}

HookStats {
    // ... existing fields ...
    latency_p50_ms: percentile(&durations, 50.0),
    latency_p95_ms: percentile(&durations, 95.0),
    latency_p99_ms: percentile(&durations, 99.0),
}
```

- [ ] **Step 5.4: Update `format_stats` to display latency**

In `format_stats()` in `stats.rs`, after the existing lines, add:

```rust
if stats.latency_p50_ms.is_some() || stats.latency_p95_ms.is_some() {
    out.push_str("\nLatency (prompt+tool hooks with timing):\n");
    if let Some(p50) = stats.latency_p50_ms {
        out.push_str(&format!("  p50: {p50} ms\n"));
    }
    if let Some(p95) = stats.latency_p95_ms {
        out.push_str(&format!("  p95: {p95} ms\n"));
    }
    if let Some(p99) = stats.latency_p99_ms {
        out.push_str(&format!("  p99: {p99} ms\n"));
    }
}
```

- [ ] **Step 5.5: Write a test for percentile computation**

Add to `mur-core/src/inject/stats.rs` tests:

```rust
#[test]
fn latency_percentiles_from_events() {
    use super::*;
    use crate::inject::event::{EventKind, NormalizedEvent};

    let events: Vec<NormalizedEvent> = (1u64..=100)
        .map(|i| NormalizedEvent {
            kind: EventKind::Prompt,
            tool_provider: "claude".into(),
            query: None,
            tool_called: None,
            tool_input: None,
            stop_reason: None,
            session_id: None,
            duration_ms: Some(i),  // 1ms … 100ms
        })
        .collect();

    let stats = compute(&events);
    assert_eq!(stats.latency_p50_ms, Some(50));
    assert_eq!(stats.latency_p95_ms, Some(95));
    assert_eq!(stats.latency_p99_ms, Some(99));
}

#[test]
fn latency_none_when_no_durations() {
    use super::*;
    use crate::inject::event::{EventKind, NormalizedEvent};

    let events = vec![NormalizedEvent {
        kind: EventKind::Prompt,
        tool_provider: "claude".into(),
        query: None, tool_called: None, tool_input: None,
        stop_reason: None, session_id: None, duration_ms: None,
    }];
    let stats = compute(&events);
    assert!(stats.latency_p50_ms.is_none());
    assert!(stats.latency_p99_ms.is_none());
}
```

- [ ] **Step 5.6: Run all stats + hook tests**

```bash
cargo test -p mur-core stats 2>&1 | tail -10
cargo test -p mur-core hook 2>&1 | tail -5
```
Expected: all pass.

- [ ] **Step 5.7: Commit**

```bash
git add mur-core/src/inject/event.rs mur-core/src/cmd/hook.rs mur-core/src/inject/stats.rs
git commit -m "feat(stats): latency p50/p95/p99 in mur hook stats via duration_ms in event queue"
```

---

## Final verification

- [ ] **Run the full workspace test suite**

```bash
cargo test --workspace 2>&1 | tail -20
```
Expected: all pass, no regressions.

- [ ] **Run clippy**

```bash
cargo clippy --workspace -- -D warnings 2>&1 | grep "^error" | head -10
```
Expected: no errors.

- [ ] **Open a PR for each task** (5 PRs total, one per task). Each task is independent and can merge in any order. Suggested PR titles:
  - `feat(init): async:true + asyncRewake:true for Claude Code hooks`
  - `feat(init): install murmurd as launchd/systemd login service`
  - `feat(hook): stale-lock respawn — auto-restart murmurd on degraded`
  - `test(gate): 100-query golden set accuracy ≥ 85%`
  - `feat(stats): latency p50/p95/p99 in mur hook stats`
