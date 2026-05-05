# mur Hooks M3 — `murmurd` Daemon Implementation Plan

**Goal:** Build `murmurd` — a long-running Tokio daemon that owns all heavy hook work. It tails `~/.mur/queue/events.jsonl`, pre-computes pattern retrieval results for each session, and writes `~/.mur/inbox/<session>.md`. `mur hook prompt` reads the inbox first (< 5 ms) and falls back to synchronous L1 retrieval only when the inbox is stale or the daemon is down.

**Architecture:** New `mur-daemon/` crate added to the workspace. Compiles to the `murmurd` binary. Depends on `mur-core` as a library for `YamlStore`, `score_and_rank`, `inject::index`, and `spawn_background_pipeline` logic. Five tasks, ~1200 LOC total.

```
~/.mur/queue/events.jsonl   ← mur hook writes (append-only)
         │
         │ tail poll (1-second interval)
         ▼
    murmurd worker
         │
         ├─ Prompt event  → score_and_rank → ~/.mur/inbox/<session>.md
         └─ Stop event    → spawn sync / evolve / emerge (replaces spawn_background_pipeline)

~/.mur/murmurd.lock          ← {pid, started_at, heartbeat_at}
~/.mur/queue/offset          ← u64 byte offset into events.jsonl

mur hook prompt:
  1. read lock → check heartbeat_at (< 30 s = daemon healthy)
  2. read inbox → check mtime (< 5 min = fresh)
  3. if healthy + fresh: print inbox, exit in < 5 ms
  4. else: synchronous L1 fallback (existing path)
```

**Tech stack:** Rust 2024, Tokio (rt-multi-thread, time, fs, process, signal), anyhow, serde_json, chrono, dirs. No `notify` crate needed — 1-second polling is sufficient and simpler.

---

## Task 1: `mur-daemon` crate scaffold + lockfile

**Files:**
- Create: `mur-daemon/Cargo.toml`
- Create: `mur-daemon/src/main.rs`
- Create: `mur-daemon/src/lock.rs`
- Modify: `Cargo.toml` (root) — add `mur-daemon` to workspace members

### Step 1: Write failing test

Create `mur-daemon/src/lock.rs` with just the test first:

```rust
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockState {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
}

pub fn lock_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("murmurd.lock")
}

pub fn write_lock(path: &Path, state: &LockState) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

pub fn read_lock(path: &Path) -> Result<Option<LockState>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Returns true if the lock is fresh (heartbeat < 30 s ago).
pub fn is_healthy(state: &LockState) -> bool {
    let age = Utc::now()
        .signed_duration_since(state.heartbeat_at)
        .num_seconds();
    age < 30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_lock_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let state = LockState {
            pid: std::process::id(),
            started_at: Utc::now(),
            heartbeat_at: Utc::now(),
        };
        write_lock(&path, &state).unwrap();
        let loaded = read_lock(&path).unwrap().unwrap();
        assert_eq!(loaded.pid, state.pid);
    }

    #[test]
    fn read_lock_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.lock");
        assert!(read_lock(&path).unwrap().is_none());
    }

    #[test]
    fn is_healthy_true_for_fresh_heartbeat() {
        let state = LockState {
            pid: 1,
            started_at: Utc::now(),
            heartbeat_at: Utc::now(),
        };
        assert!(is_healthy(&state));
    }

    #[test]
    fn is_healthy_false_for_stale_heartbeat() {
        use chrono::Duration;
        let state = LockState {
            pid: 1,
            started_at: Utc::now(),
            heartbeat_at: Utc::now() - Duration::seconds(60),
        };
        assert!(!is_healthy(&state));
    }
}
```

### Step 2: Create `mur-daemon/Cargo.toml`

```toml
[package]
name = "mur-daemon"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "murmurd"
path = "src/main.rs"

[dependencies]
mur-core = { path = "../mur-core" }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
anyhow = { workspace = true }
dirs = "6"
tempfile = "3"
```

### Step 3: Create `mur-daemon/src/main.rs` (minimal scaffold)

```rust
mod lock;

use anyhow::Result;
use chrono::Utc;
use lock::{LockState, lock_path, write_lock};

#[tokio::main]
async fn main() -> Result<()> {
    let lock_file = lock_path();
    let state = LockState {
        pid: std::process::id(),
        started_at: Utc::now(),
        heartbeat_at: Utc::now(),
    };
    write_lock(&lock_file, &state)?;
    println!("murmurd started (pid {})", state.pid);
    Ok(())
}
```

### Step 4: Add to workspace

In root `Cargo.toml`, change:
```toml
members = [
    "mur-common",
    "mur-core",
    "mur-agent-runtime",
]
```
to:
```toml
members = [
    "mur-common",
    "mur-core",
    "mur-agent-runtime",
    "mur-daemon",
]
```

### Step 5: Run tests

```bash
cargo test -p mur-daemon 2>&1 | tail -15
```
Expected: 4 passed.

### Step 6: Build + clippy + commit

```bash
cargo build -p mur-daemon 2>&1 | grep "^error" | head -5
cargo clippy -p mur-daemon -- -D warnings 2>&1 | grep "^error" | head -5
git add mur-daemon/ Cargo.toml Cargo.lock
git commit -m "feat(daemon): mur-daemon crate scaffold + LockState read/write/health"
```

---

## Task 2: Queue tail consumer

**Files:**
- Create: `mur-daemon/src/consumer.rs`
- Modify: `mur-daemon/src/main.rs` — add `mod consumer;`

### Step 1: Write failing tests

Create `mur-daemon/src/consumer.rs`:

```rust
use anyhow::Result;
use mur_core::inject::event::NormalizedEvent;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub fn queue_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("queue")
        .join("events.jsonl")
}

pub fn offset_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("queue")
        .join("offset")
}

/// Read byte offset from disk; returns 0 if file missing.
pub fn read_offset(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Persist current byte offset to disk.
pub fn write_offset(path: &Path, offset: u64) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, offset.to_string())?;
    Ok(())
}

/// Read all new events from `queue_file` starting at `start_offset`.
/// Returns (new_events, new_offset).
pub fn drain_new(queue_file: &Path, start_offset: u64) -> Result<(Vec<NormalizedEvent>, u64)> {
    if !queue_file.exists() {
        return Ok((vec![], start_offset));
    }
    let mut f = std::fs::File::open(queue_file)?;
    f.seek(SeekFrom::Start(start_offset))?;

    let mut events = Vec::new();
    let mut new_offset = start_offset;
    let reader = BufReader::new(&f);

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        new_offset += line.len() as u64 + 1; // +1 for newline
        if let Ok(ev) = serde_json::from_str::<NormalizedEvent>(line) {
            events.push(ev);
        }
    }

    Ok((events, new_offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_core::inject::event::{EventKind, NormalizedEvent};

    fn write_events(path: &Path, events: &[NormalizedEvent]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for ev in events {
            writeln!(f, "{}", serde_json::to_string(ev).unwrap()).unwrap();
        }
    }

    fn make_event(query: &str) -> NormalizedEvent {
        NormalizedEvent {
            kind: EventKind::Prompt,
            tool_provider: "claude".into(),
            query: Some(query.into()),
            tool_called: None,
            tool_input: None,
            stop_reason: None,
            session_id: Some("sess1".into()),
        }
    }

    #[test]
    fn drain_reads_all_from_offset_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        write_events(&path, &[make_event("q1"), make_event("q2")]);
        let (events, new_off) = drain_new(&path, 0).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].query.as_deref(), Some("q1"));
        assert!(new_off > 0);
    }

    #[test]
    fn drain_reads_only_new_events_from_saved_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        write_events(&path, &[make_event("old")]);
        let (_, off1) = drain_new(&path, 0).unwrap();
        // Add a new event
        write_events(&path, &[make_event("new")]);
        let (events, _) = drain_new(&path, off1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].query.as_deref(), Some("new"));
    }

    #[test]
    fn drain_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let (events, off) = drain_new(&path, 0).unwrap();
        assert!(events.is_empty());
        assert_eq!(off, 0);
    }

    #[test]
    fn offset_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offset");
        write_offset(&path, 12345).unwrap();
        assert_eq!(read_offset(&path), 12345);
    }

    #[test]
    fn read_offset_missing_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        assert_eq!(read_offset(&path), 0);
    }
}
```

### Step 2: Run tests

```bash
cargo test -p mur-daemon consumer 2>&1 | tail -15
```
Expected: 5 passed.

### Step 3: Add `mod consumer;` to `main.rs`

```rust
mod consumer;
mod lock;
```

### Step 4: Clippy + fmt + commit

```bash
cargo clippy -p mur-daemon -- -D warnings 2>&1 | grep "^error" | head -5
cargo fmt -p mur-daemon
git add mur-daemon/src/consumer.rs mur-daemon/src/main.rs
git commit -m "feat(daemon): queue tail consumer — drain_new with byte-offset tracking"
```

---

## Task 3: Inbox writer

**Files:**
- Create: `mur-daemon/src/inbox.rs`
- Modify: `mur-daemon/src/main.rs` — add `mod inbox;`

### Step 1: Write failing tests

```rust
// mur-daemon/src/inbox.rs
use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn inbox_path(session_id: &str) -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("inbox")
        .join(format!("{session_id}.md"))
}

/// Write pre-computed context to the inbox file for a session.
pub fn write_inbox(path: &Path, content: &str) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

/// Read inbox content; returns None if missing or older than `max_age_secs`.
pub fn read_inbox(path: &Path, max_age_secs: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .ok()?;
    if age.as_secs() > max_age_secs {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_inbox_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        write_inbox(&path, "## context\n- foo — bar\n").unwrap();
        let content = read_inbox(&path, 300).unwrap();
        assert!(content.contains("foo"));
    }

    #[test]
    fn read_inbox_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.md");
        assert!(read_inbox(&path, 300).is_none());
    }

    #[test]
    fn read_inbox_stale_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.md");
        write_inbox(&path, "stale content").unwrap();
        // max_age = 0 → always stale
        assert!(read_inbox(&path, 0).is_none());
    }
}
```

### Step 2: Run tests

```bash
cargo test -p mur-daemon inbox 2>&1 | tail -10
```
Expected: 3 passed.

### Step 3: Add `mod inbox;` to `main.rs`

### Step 4: Implement `process_event` function in `main.rs`

Add to `main.rs` (before `main()`):

```rust
use mur_core::inject::index::{build as build_index, format_l0};
use mur_core::store::yaml::YamlStore;
use mur_core::inject::event::{EventKind, NormalizedEvent};

const L0_BUDGET_CHARS: usize = 2400;
const INBOX_TTL_SECS: u64 = 300;

fn process_event(event: &NormalizedEvent) -> Result<()> {
    match event.kind {
        EventKind::Prompt => {
            let Some(ref session_id) = event.session_id else {
                return Ok(());
            };
            let yaml_store = YamlStore::default_store()?;
            let patterns = yaml_store.list_all()?;
            let index = build_index(&patterns, None);
            let content = format_l0(&index, L0_BUDGET_CHARS);
            if !content.is_empty() {
                let path = inbox::inbox_path(session_id);
                inbox::write_inbox(&path, &content)?;
            }
        }
        EventKind::Stop => {
            // Spawn heavy background work (replaces spawn_background_pipeline in hook.rs)
            let mur_bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("mur")))
                .unwrap_or_else(|| std::path::PathBuf::from("mur"));
            for subcmd in &["sync", "evolve", "emerge"] {
                let _ = std::process::Command::new(&mur_bin)
                    .arg(subcmd)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
        _ => {}
    }
    Ok(())
}
```

### Step 5: Build + clippy + commit

```bash
cargo build -p mur-daemon 2>&1 | grep "^error" | head -5
cargo clippy -p mur-daemon -- -D warnings 2>&1 | grep "^error" | head -5
cargo fmt -p mur-daemon
git add mur-daemon/src/inbox.rs mur-daemon/src/main.rs
git commit -m "feat(daemon): inbox writer + process_event (Prompt→inbox, Stop→background jobs)"
```

---

## Task 4: Event loop + heartbeat + `mur hook prompt` inbox reader

**Files:**
- Modify: `mur-daemon/src/main.rs` — full async event loop + heartbeat
- Modify: `mur-core/src/cmd/hook.rs` — inbox-first prompt handler
- Create: `mur-core/tests/hook_inbox_integration.rs`

### Step 1: Full event loop in `main.rs`

Replace the stub `main()` with:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let lock_file = lock_path();

    // Steal stale lock or bail if another healthy instance is running
    if let Some(existing) = lock::read_lock(&lock_file)? {
        if lock::is_healthy(&existing) {
            eprintln!("murmurd already running (pid {})", existing.pid);
            std::process::exit(1);
        }
    }

    let started = Utc::now();
    let state = LockState {
        pid: std::process::id(),
        started_at: started,
        heartbeat_at: started,
    };
    write_lock(&lock_file, &state)?;

    let queue_file = consumer::queue_path();
    let offset_file = consumer::offset_path();
    let mut offset = consumer::read_offset(&offset_file);

    // Heartbeat: update lock every 10 seconds
    let lock_file_hb = lock_file.clone();
    let pid = state.pid;
    let started_hb = state.started_at;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let new_state = LockState {
                pid,
                started_at: started_hb,
                heartbeat_at: Utc::now(),
            };
            let _ = lock::write_lock(&lock_file_hb, &new_state);
        }
    });

    // Event loop: poll queue every second
    let mut poll = tokio::time::interval(tokio::time::Duration::from_secs(1));
    loop {
        poll.tick().await;
        let (events, new_offset) = consumer::drain_new(&queue_file, offset)?;
        if new_offset > offset {
            consumer::write_offset(&offset_file, new_offset)?;
            offset = new_offset;
        }
        for event in events {
            if let Err(e) = process_event(&event) {
                eprintln!("murmurd: process_event error: {e:#}");
            }
        }
    }
}
```

### Step 2: Update `mur hook prompt` to read inbox first

In `mur-core/src/cmd/hook.rs`, replace `cmd_hook_prompt` to add inbox-first path:

```rust
pub(crate) async fn cmd_hook_prompt(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw.clone(), EventKind::Prompt, tool);
    let _ = enqueue(&event);

    let query = extract_query(&raw).unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(());
    }

    let inputs = GateInputs::default();
    let outcome = evaluate_query_v2(&query, &inputs);
    if outcome.tier == GateTier::Skip {
        return Ok(());
    }

    // Inbox-first path: check if daemon has pre-computed context
    if let Some(session_id) = event.session_id.as_deref() {
        let inbox = crate::daemon::inbox_path(session_id);
        if let Some(content) = crate::daemon::read_inbox(&inbox, 300) {
            print!("{content}");
            return Ok(());
        }
    }

    // Degraded-mode / cold-start fallback: synchronous retrieval
    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    use mur_common::pattern::LifecycleStatus;
    let injected: Vec<_> = score_and_rank(&query, patterns)
        .into_iter()
        .filter(|sp| sp.pattern.lifecycle.status != LifecycleStatus::Archived)
        .map(|sp| sp.pattern)
        .collect();

    let budget = match outcome.tier {
        GateTier::L0 => 300,
        GateTier::L1 => 500,
        GateTier::L2 => 2000,
        GateTier::Skip => unreachable!(),
    };

    let output = crate::inject::hook::format_unified_injection_with_store(
        &injected,
        &workflows,
        budget,
        Some(&yaml_store),
    );

    if !output.is_empty() {
        print!("{output}");
    }
    Ok(())
}
```

Add a thin re-export module `mur-core/src/daemon.rs`:

```rust
//! Thin re-exports so hook.rs can reach inbox helpers without depending on mur-daemon.

use std::path::{Path, PathBuf};

pub fn inbox_path(session_id: &str) -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("inbox")
        .join(format!("{session_id}.md"))
}

pub fn read_inbox(path: &Path, max_age_secs: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .ok()?;
    if age.as_secs() > max_age_secs {
        return None;
    }
    std::fs::read_to_string(path).ok()
}
```

Add to `mur-core/src/lib.rs`:
```rust
pub mod daemon;
```

### Step 3: Write integration test

Create `mur-core/tests/hook_inbox_integration.rs`:

```rust
//! Tests the inbox read helpers used by mur hook prompt.
use mur_core::daemon::{inbox_path, read_inbox};

#[test]
fn fresh_inbox_is_returned() {
    let dir = tempfile::tempdir().unwrap();
    // Simulate writing inbox directly
    let path = dir.path().join("test_session.md");
    std::fs::write(&path, "## mur context\n- foo — bar\n").unwrap();
    let content = read_inbox(&path, 300).unwrap();
    assert!(content.contains("mur context"));
}

#[test]
fn missing_inbox_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.md");
    assert!(read_inbox(&path, 300).is_none());
}

#[test]
fn stale_inbox_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale.md");
    std::fs::write(&path, "old content").unwrap();
    assert!(read_inbox(&path, 0).is_none());
}
```

### Step 4: Run all tests

```bash
cargo test --workspace 2>&1 | grep -E "FAILED|test result" | tail -15
```
Expected: zero failures.

### Step 5: Clippy + fmt + commit

```bash
cargo clippy --workspace -- -D warnings 2>&1 | grep "^error" | head -5
cargo fmt --check 2>&1 || cargo fmt
git add mur-daemon/src/main.rs mur-core/src/cmd/hook.rs mur-core/src/daemon.rs mur-core/src/lib.rs mur-core/tests/hook_inbox_integration.rs
git commit -m "feat(daemon): event loop + heartbeat + mur hook prompt inbox-first path"
```

---

## Task 5: `mur murmurd` CLI subcommand

**Files:**
- Modify: `mur-core/src/cmd/mod.rs` — add `murmurd` command
- Create: `mur-core/src/cmd/murmurd.rs` — start / stop / status

### Step 1: Create `mur-core/src/cmd/murmurd.rs`

```rust
use anyhow::Result;
use crate::daemon::{inbox_path as _, read_inbox as _};

pub fn cmd_murmurd_status() -> Result<()> {
    let lock_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur")
        .join("murmurd.lock");

    match std::fs::read_to_string(&lock_path) {
        Ok(s) => {
            let state: serde_json::Value = serde_json::from_str(&s)?;
            let pid = state.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            let hb = state
                .get("heartbeat_at")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!("murmurd running (pid {pid}, last heartbeat {hb})");
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("murmurd not running (no lock file)");
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn cmd_murmurd_stop() -> Result<()> {
    let lock_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur")
        .join("murmurd.lock");

    let s = match std::fs::read_to_string(&lock_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("murmurd not running");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let state: serde_json::Value = serde_json::from_str(&s)?;
    let pid = state
        .get("pid")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("malformed lock"))?;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .spawn();
    }

    let _ = std::fs::remove_file(&lock_path);
    println!("murmurd stopped (sent SIGTERM to pid {pid})");
    Ok(())
}

pub fn cmd_murmurd_start(detach: bool) -> Result<()> {
    // Find murmurd binary next to current executable
    let murmurd = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("murmurd")))
        .unwrap_or_else(|| std::path::PathBuf::from("murmurd"));

    if !murmurd.exists() {
        anyhow::bail!(
            "murmurd binary not found at {}. Build with: cargo build -p mur-daemon",
            murmurd.display()
        );
    }

    let mut cmd = std::process::Command::new(&murmurd);
    if detach {
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd.spawn()?;
        println!("murmurd started in background");
    } else {
        let status = cmd.status()?;
        if !status.success() {
            anyhow::bail!("murmurd exited with: {status}");
        }
    }
    Ok(())
}
```

### Step 2: Wire into the CLI

In `mur-core/src/cmd/mod.rs` (or wherever `Cli` is defined), add:
```
murmurd start [--detach]
murmurd stop
murmurd status
```

### Step 3: Run full workspace test suite

```bash
cargo test --workspace 2>&1 | grep -E "FAILED|test result" | tail -10
```

### Step 4: Clippy + fmt + commit

```bash
cargo clippy --workspace -- -D warnings 2>&1 | grep "^error" | head -5
cargo fmt --check 2>&1 || cargo fmt
git add mur-core/src/cmd/murmurd.rs mur-core/src/cmd/mod.rs
git commit -m "feat(cmd): mur murmurd start/stop/status CLI subcommand"
```

---

## Notes for the implementer

- **`mur-core` as a library for `mur-daemon`**: `mur-core` is both a binary (`[[bin]]`) and a library (`[lib]`). `mur-daemon` imports it as `mur-core = { path = "../mur-core" }`. Check that `mur-core/src/lib.rs` exports `store::yaml::YamlStore`, `inject::index`, `inject::event::NormalizedEvent`. If `YamlStore` is not re-exported, add `pub use store::yaml::YamlStore;` or use the full path.

- **inbox duplication**: `inbox_path` and `read_inbox` appear in both `mur-daemon/src/inbox.rs` and `mur-core/src/daemon.rs`. This is intentional: `mur-daemon` owns the full inbox module; `mur-core` has a thin re-export so `hook.rs` doesn't pull in a circular dependency. If a future refactor extracts `mur-common` inbox types, remove the duplication then.

- **`spawn_background_pipeline` in `hook.rs`**: After M3 is complete and `murmurd` is running, the `Stop` handler in the daemon covers sync/evolve/emerge. The `spawn_background_pipeline` call in `cmd_hook_stop` should eventually check lockfile health and skip if daemon is running; for now, leave it as a fallback — double-spawning is idempotent for these operations.

- **`mur-core` `[lib]` vs `[[bin]]`**: Check `mur-core/Cargo.toml` to confirm `[lib]` section exists. If the crate is bin-only, add:
  ```toml
  [lib]
  name = "mur_core"
  path = "src/lib.rs"
  ```

- **Signal handling on macOS**: `kill <pid>` sends SIGTERM which is graceful. The Tokio event loop in `main.rs` does not currently catch SIGTERM. For production, add `tokio::signal::unix::signal(SignalKind::terminate())` to the loop. For now, lock-file deletion on stop is sufficient.
