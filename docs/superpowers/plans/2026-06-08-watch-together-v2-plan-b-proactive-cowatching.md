# Watch-Together v2 — Plan B: Proactive Co-Watching (runtime-only) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** While MuR runs as a murmur agent runtime, detect big on-screen scene changes in
VLC and proactively inject a short, rate-limited, consent-gated, silenceable interjection
("watch buddy"), delivered through the agent's normal output channel.

**Architecture:** Because the crate dependency edge is **`mur-core → mur-agent-runtime`**,
the runtime is the lower layer and cannot call `mur-core`. So: shared serde types
(`VlcRuntime`, `WatchSession`) live in **`mur-common`**; the runtime's `WatchScheduler`
(a sibling of `IdleScheduler`) takes its own VLC snapshot over HTTP, computes a dHash,
gates on a pure `should_interject`, and injects a **plain-text turn** into `TaskRunner`
— the agent then calls `scene_explain` itself. The MCP `watch_*` tools only flip
`watch.json` flags. Pure logic (session I/O, dHash core, gate) is TDD'd; VLC HTTP / the
scheduler loop are build-only + manual E2E.

**Tech Stack:** Rust 2024. `mur-common` (serde types + atomic file I/O, like `lock_file`).
`mur-agent-runtime` (already has `tokio` + `reqwest`; **adds `image`** for PNG dHash).
`mur-core` (session-mutator wrappers) + `mur-mcp-server` (`watch_*` tools).

**Spec:** `docs/superpowers/specs/2026-06-08-watch-together-v2-design.md` (§3, §6, §9 Plan B).

**Prerequisite:** Plan A is **not** required to compile Plan B, but they share the media
area; land Plan A first if doing both. Plan B only manifests when MuR runs as a runtime
agent with a delivery channel (§3).

---

## File Structure

**Create:**
- `mur-common/src/media.rs` — `VlcRuntime` (moved from `mur-core`), `runtime_path`;
  `WatchSession`, `Consent`, `watch_path`, `load_watch`, `save_watch`.
- `mur-core/src/cmd/media/watch.rs` — session-mutator fns behind the MCP tools.
- `mur-agent-runtime/src/watch_scheduler.rs` — dHash, `should_interject`, capture loop.
- `mur-core/src/skills/watch_together.yaml` — skill manifest.

**Modify:**
- `mur-common/src/lib.rs` — `pub mod media;`.
- `mur-core/src/cmd/media/mod.rs` — re-export `VlcRuntime`/`runtime_path` from
  `mur_common::media`; declare `pub mod watch;`.
- `mur-agent-runtime/Cargo.toml` — add `image`.
- `mur-agent-runtime/src/lib.rs` — `pub mod watch_scheduler;` (beside `idle_scheduler`).
- `mur-agent-runtime/src/supervisor.rs` — spawn `WatchScheduler`.
- `mur-mcp-server/src/tools.rs` — register `watch_*` tools + dispatch + test.
- `mur-core/src/cmd/sync_cmd.rs` — register the `watch-together` skill.

---

## Phase 0 — Shared types in mur-common

### Task 1: Move `VlcRuntime` to `mur-common::media` (behavior-preserving)

**Files:**
- Create: `mur-common/src/media.rs`
- Modify: `mur-common/src/lib.rs`, `mur-core/src/cmd/media/mod.rs`

- [ ] **Step 1: Create the mur-common media module with VlcRuntime**

Create `mur-common/src/media.rs`:

```rust
//! Shared media runtime types (no business logic). Consumed by both `mur-core`
//! (VLC control, media tools) and `mur-agent-runtime` (WatchScheduler).

use serde::{Deserialize, Serialize};
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

/// Path to the persisted VLC runtime config.
pub fn runtime_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("vlc.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_path_under_runtime_dir() {
        let p = runtime_path(Path::new("/tmp/h"));
        assert!(p.ends_with("runtime/vlc.json"));
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-common/src/lib.rs`, add `pub mod media;` (keep the list alphabetical — it goes
between `pub mod manifest;` and `pub mod mobile;`).

- [ ] **Step 3: Re-export from mur-core and delete the local definitions**

In `mur-core/src/cmd/media/mod.rs`:
1. Delete the local `pub struct VlcRuntime { … }` definition and the
   `pub fn runtime_path(…) { … }` function.
2. Add a re-export near the top (after the existing `use` lines):

```rust
pub use mur_common::media::{runtime_path, VlcRuntime};
```

Leave `load_runtime`, `save_runtime`, `pick_free_port`, `gen_password`, and
`shared_client` as they are — they now operate on the re-exported `VlcRuntime` /
`runtime_path` and continue to compile unchanged.

3. If the compiler/clippy then flags `use serde::{Deserialize, Serialize};` in
   `mod.rs` as unused (the moved struct was its only user), remove that line. Keep it if
   other items in `mod.rs` still derive serde.

- [ ] **Step 4: Verify both crates build and existing media tests pass**

Run: `cargo build -p mur-common -p mur-core`
Expected: builds.

Run: `cargo test -p mur-common media::tests`
Expected: PASS (1 test).

Run: `cargo test -p mur-core media::mod`
Expected: PASS — the existing v1 `password_is_32_hex_chars` and `runtime_roundtrips`
tests still pass against the re-exported type.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/media.rs mur-common/src/lib.rs mur-core/src/cmd/media/mod.rs
git commit -m "refactor(media): move VlcRuntime to mur-common (shared with runtime)"
```

---

### Task 2: `WatchSession` + `Consent` + load/save in mur-common

**Files:**
- Modify: `mur-common/src/media.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-common/src/media.rs`:

```rust
/// Whether the user has agreed to proactive interjections this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Consent {
    #[default]
    Unasked,
    Granted,
    Declined,
}

/// Persisted proactive-watch session state. Written by the MCP `watch_*` tools
/// (via `mur-core`) and read by the runtime `WatchScheduler`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WatchSession {
    pub active: bool,
    pub muted: bool,
    pub last_interjection_ms: i64,
    pub last_scene_phash: u64,
    pub consent: Consent,
}

/// Path to the persisted watch session.
pub fn watch_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("watch.json")
}

/// Load the watch session, or a default (all-off) session if absent/unparseable.
pub fn load_watch(mur_home: &Path) -> WatchSession {
    std::fs::read_to_string(watch_path(mur_home))
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
        .unwrap_or_default()
}

/// Persist the watch session atomically (temp + rename).
pub fn save_watch(mur_home: &Path, s: &WatchSession) -> std::io::Result<()> {
    let path = watch_path(mur_home);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(s).expect("serialize WatchSession");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod watch_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn absent_session_is_default_off() {
        let home = TempDir::new().unwrap();
        let s = load_watch(home.path());
        assert!(!s.active);
        assert_eq!(s.consent, Consent::Unasked);
    }

    #[test]
    fn session_roundtrips() {
        let home = TempDir::new().unwrap();
        let s = WatchSession {
            active: true,
            muted: false,
            last_interjection_ms: 123,
            last_scene_phash: 0xABCD,
            consent: Consent::Granted,
        };
        save_watch(home.path(), &s).unwrap();
        assert_eq!(load_watch(home.path()), s);
    }
}
```

- [ ] **Step 2: Ensure `tempfile` is a dev-dependency of mur-common**

Run: `grep -n "tempfile" mur-common/Cargo.toml`
Expected: present under `[dev-dependencies]`. If absent, add `tempfile = "3"` there.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p mur-common media::watch_tests`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/media.rs mur-common/Cargo.toml
git commit -m "feat(media): WatchSession/Consent shared types + atomic load/save"
```

---

## Phase 1 — Session mutators + MCP tools + skill (mur-core / mur-mcp-server)

### Task 3: `media/watch.rs` session mutators

**Files:**
- Create: `mur-core/src/cmd/media/watch.rs`
- Modify: `mur-core/src/cmd/media/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/media/watch.rs`:

```rust
//! Watch-session mutators behind the MCP `watch_*` tools. These only flip flags in
//! `watch.json`; the runtime `WatchScheduler` observes them (spec §3, §6).

use mur_common::media::{load_watch, save_watch, Consent, WatchSession};
use std::path::Path;

/// Start (or restart) a proactive watch session: active, unmuted, consent reset.
pub fn start(mur_home: &Path) -> std::io::Result<WatchSession> {
    let mut s = load_watch(mur_home);
    s.active = true;
    s.muted = false;
    s.consent = Consent::Unasked;
    s.last_interjection_ms = 0;
    s.last_scene_phash = 0;
    save_watch(mur_home, &s)?;
    Ok(s)
}

/// Stop the session (no further interjections).
pub fn stop(mur_home: &Path) -> std::io::Result<WatchSession> {
    let mut s = load_watch(mur_home);
    s.active = false;
    save_watch(mur_home, &s)?;
    Ok(s)
}

/// Silence interjections ("噓") without ending the session.
pub fn mute(mur_home: &Path) -> std::io::Result<WatchSession> {
    let mut s = load_watch(mur_home);
    s.muted = true;
    save_watch(mur_home, &s)?;
    Ok(s)
}

/// Current session snapshot.
pub fn status(mur_home: &Path) -> WatchSession {
    load_watch(mur_home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn start_then_mute_then_stop() {
        let home = TempDir::new().unwrap();
        let s = start(home.path()).unwrap();
        assert!(s.active && !s.muted);
        let s = mute(home.path()).unwrap();
        assert!(s.active && s.muted);
        let s = stop(home.path()).unwrap();
        assert!(!s.active && s.muted);
        assert_eq!(status(home.path()).active, false);
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-core/src/cmd/media/mod.rs`, add `pub mod watch;`.

- [ ] **Step 3: Run the test**

Run: `cargo test -p mur-core media::watch::tests`
Expected: PASS (1 test).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/media/watch.rs mur-core/src/cmd/media/mod.rs
git commit -m "feat(media): watch session mutators (start/stop/mute/status)"
```

---

### Task 4: Register `watch_*` MCP tools

**Files:**
- Modify: `mur-mcp-server/src/tools.rs`

- [ ] **Step 1: Add tool definitions**

In `mur-mcp-server/src/tools.rs`, after the `video_analyze` `Tool { … }` block from
Plan A (or after `scene_explain` if Plan A is not landed), add:

```rust
        Tool {
            name: "watch_start".into(),
            description: "Begin a proactive co-watching session: MuR may briefly comment on big scene changes (runtime-only; consent-gated; say \"噓\" to mute).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "watch_stop".into(),
            description: "End the proactive co-watching session.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "watch_mute".into(),
            description: "Silence proactive interjections without ending the session (\"噓\").".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
        Tool {
            name: "watch_status".into(),
            description: "Report the current co-watching session state (active/muted/consent).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        },
```

- [ ] **Step 2: Add dispatch arms**

In `call_tool`, after the `scene_explain` (or `video_analyze`) arm, add:

```rust
        "watch_start" => {
            let home = resolve_mur_home().map_err(|e| format!("watch_start failed: {e}"))?;
            let s = mur_core::cmd::media::watch::start(&home)
                .map_err(|e| format!("watch_start failed: {e}"))?;
            Ok(serde_json::to_value(s).unwrap_or(Value::Null))
        }
        "watch_stop" => {
            let home = resolve_mur_home().map_err(|e| format!("watch_stop failed: {e}"))?;
            let s = mur_core::cmd::media::watch::stop(&home)
                .map_err(|e| format!("watch_stop failed: {e}"))?;
            Ok(serde_json::to_value(s).unwrap_or(Value::Null))
        }
        "watch_mute" => {
            let home = resolve_mur_home().map_err(|e| format!("watch_mute failed: {e}"))?;
            let s = mur_core::cmd::media::watch::mute(&home)
                .map_err(|e| format!("watch_mute failed: {e}"))?;
            Ok(serde_json::to_value(s).unwrap_or(Value::Null))
        }
        "watch_status" => {
            let home = resolve_mur_home().map_err(|e| format!("watch_status failed: {e}"))?;
            let s = mur_core::cmd::media::watch::status(&home);
            Ok(serde_json::to_value(s).unwrap_or(Value::Null))
        }
```

(`resolve_mur_home()` is the existing helper at the bottom of `tools.rs`.)

- [ ] **Step 3: Extend the registration test**

Update `media_tools_registered` so the loop also asserts the watch tools:

```rust
        for n in [
            "vlc_open", "vlc_playback", "vlc_status", "scene_explain",
            "watch_start", "watch_stop", "watch_mute", "watch_status",
        ] {
```

(Include `"video_analyze"` too if Plan A is landed.)

- [ ] **Step 4: Run the test + build**

Run: `cargo test -p mur-mcp-server media_tool_tests`
Expected: PASS.

Run: `cargo build -p mur-mcp-server`
Expected: builds.

- [ ] **Step 5: Commit**

```bash
git add mur-mcp-server/src/tools.rs
git commit -m "feat(mcp): register watch_* session tools"
```

---

### Task 5: `watch-together` skill manifest

**Files:**
- Create: `mur-core/src/skills/watch_together.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs`

- [ ] **Step 1: Create the manifest**

Create `mur-core/src/skills/watch_together.yaml`:

```yaml
name: watch-together
version: 0.1.0
publisher: human:mur
description: "Co-watch a video with the user: start a session, comment briefly on scene changes when invited, and mute on request."
category: media
hosts: [all]
content:
  abstract: |
    When the user wants to watch together (not just control playback), call watch_start.
    During a session, MuR may inject a short prompt when the scene changes — explain the
    frame briefly with scene_explain. If the user says "噓"/"安靜"/"stop talking", call
    watch_mute. watch_stop ends the session. Proactive comments only happen when MuR runs
    as a background agent runtime.
  context: |
    # watch-together — be a gentle watch buddy

    - watch_start(): begin a proactive session (asks consent before the first comment).
    - watch_mute(): silence interjections immediately on "噓"/"安靜".
    - watch_stop(): end the session.
    - watch_status(): check active/muted/consent.
    Keep comments to one short sentence. Ask consent before interjecting the first time.
    Decline DRM-protected services. Everything is local; nothing is uploaded.
tags: [mur, media, video, watch, builtin]
triggers:
  - type: keyword
    pattern: "一起看|陪我看|watch (this )?(with me|together)|邊看邊|噓|安靜.{0,4}(別說|不要說)"
  - type: manual
priority: normal
```

- [ ] **Step 2: Register the skill**

In `mur-core/src/cmd/sync_cmd.rs`, in `ensure_mur_skill`'s `skills` array, after the
`("scene-explain", …)` entry (and after `("video-analyze", …)` if Plan A landed), add:

```rust
        (
            "watch-together",
            include_str!("../skills/watch_together.yaml"),
        ),
```

- [ ] **Step 3: Verify the manifest parses and the crate builds**

Run: `python3 -c "import yaml; yaml.safe_load(open('mur-core/src/skills/watch_together.yaml')); print('ok')"`
Expected: `ok`.

Run: `cargo build -p mur-core`
Expected: builds.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/skills/watch_together.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(media): watch-together skill manifest + registration"
```

---

## Phase 2 — Runtime scene detection (pure logic TDD)

### Task 6: dHash core (pure) + `image` dependency

**Files:**
- Modify: `mur-agent-runtime/Cargo.toml`
- Create: `mur-agent-runtime/src/watch_scheduler.rs`
- Modify: `mur-agent-runtime/src/lib.rs` (module root that lists `mod …;`)

- [ ] **Step 1: Add the `image` dependency**

In `mur-agent-runtime/Cargo.toml` `[dependencies]`, add (default features are fine; PNG
is included by default):

```toml
image = "0.25"
```

- [ ] **Step 2: Write the failing test**

Create `mur-agent-runtime/src/watch_scheduler.rs`:

```rust
//! C-watch — proactive co-watching scene detection (spec §6). Runtime-only.
//!
//! Takes its own VLC snapshot over HTTP, computes a difference-hash (dHash), and on a
//! large change injects a short text turn into the `TaskRunner`. Cannot call `mur-core`
//! (would cycle), so it relies on `mur_common::media` shared types + the agent's own
//! `scene_explain` tool to actually narrate.

/// Hamming distance between two 64-bit perceptual hashes.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Compute a dHash from a row-major grayscale buffer of size `w`×`h`.
/// Sets one bit per horizontally-adjacent pair (`right > left`). For 9×8 ⇒ 64 bits.
pub fn dhash_from_luma(luma: &[u8], w: usize, h: usize) -> u64 {
    let mut hash = 0u64;
    let mut bit = 0u32;
    for y in 0..h {
        for x in 0..w.saturating_sub(1) {
            let left = luma[y * w + x];
            let right = luma[y * w + x + 1];
            if right > left {
                hash |= 1u64 << bit;
            }
            bit += 1;
        }
    }
    hash
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming(0b0000, 0b0000), 0);
        assert_eq!(hamming(0b1010, 0b0001), 3);
    }

    #[test]
    fn dhash_detects_horizontal_gradient() {
        // 9x8 ascending rows ⇒ every pair has right > left ⇒ all 64 bits set.
        let mut luma = vec![0u8; 9 * 8];
        for y in 0..8 {
            for x in 0..9 {
                luma[y * 9 + x] = (x * 20) as u8;
            }
        }
        assert_eq!(dhash_from_luma(&luma, 9, 8), u64::MAX);

        // A flat image ⇒ no bit set ⇒ maximally different from the gradient.
        let flat = vec![100u8; 9 * 8];
        assert_eq!(dhash_from_luma(&flat, 9, 8), 0);
        assert_eq!(hamming(u64::MAX, 0), 64);
    }
}
```

- [ ] **Step 3: Register the module**

In `mur-agent-runtime/src/lib.rs`, beside the existing `pub mod idle_scheduler;`
(line 22), add (matching its visibility so `supervisor.rs` can reach it via
`crate::watch_scheduler`):

```rust
pub mod watch_scheduler;
```

- [ ] **Step 4: Run the tests + build**

Run: `cargo test -p mur-agent-runtime watch_scheduler::hash_tests`
Expected: PASS (2 tests).

Run: `cargo build -p mur-agent-runtime`
Expected: builds (downloads `image`).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/Cargo.toml mur-agent-runtime/src/watch_scheduler.rs mur-agent-runtime/src/lib.rs
git commit -m "feat(watch): dHash core + image dep (runtime scene detection)"
```

---

### Task 7: `should_interject` gate (pure)

**Files:**
- Modify: `mur-agent-runtime/src/watch_scheduler.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-agent-runtime/src/watch_scheduler.rs`:

```rust
use mur_common::media::Consent;

/// What the scheduler should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Narrate,
    AskConsent,
    Skip,
}

/// Pure gate — no I/O. Order of checks: mute → quiet → magnitude → cooldown → consent.
#[allow(clippy::too_many_arguments)]
pub fn should_interject(
    now_ms: i64,
    last_interjection_ms: i64,
    cooldown_ms: i64,
    distance: u32,
    threshold: u32,
    muted: bool,
    quiet_now: bool,
    consent: Consent,
) -> Decision {
    if muted || quiet_now {
        return Decision::Skip;
    }
    if distance < threshold {
        return Decision::Skip;
    }
    if last_interjection_ms != 0 && now_ms.saturating_sub(last_interjection_ms) < cooldown_ms {
        return Decision::Skip;
    }
    match consent {
        Consent::Unasked => Decision::AskConsent,
        Consent::Granted => Decision::Narrate,
        Consent::Declined => Decision::Skip,
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    const CD: i64 = 45_000;
    const TH: u32 = 18;

    #[test]
    fn small_change_skips() {
        assert_eq!(
            should_interject(100_000, 0, CD, 5, TH, false, false, Consent::Granted),
            Decision::Skip
        );
    }

    #[test]
    fn first_big_change_asks_consent() {
        assert_eq!(
            should_interject(100_000, 0, CD, 30, TH, false, false, Consent::Unasked),
            Decision::AskConsent
        );
    }

    #[test]
    fn granted_big_change_narrates() {
        assert_eq!(
            should_interject(100_000, 0, CD, 30, TH, false, false, Consent::Granted),
            Decision::Narrate
        );
    }

    #[test]
    fn cooldown_blocks() {
        // fired 10s ago, cooldown 45s ⇒ skip
        assert_eq!(
            should_interject(110_000, 100_000, CD, 30, TH, false, false, Consent::Granted),
            Decision::Skip
        );
    }

    #[test]
    fn muted_quiet_declined_all_skip() {
        assert_eq!(should_interject(200_000, 0, CD, 99, TH, true, false, Consent::Granted), Decision::Skip);
        assert_eq!(should_interject(200_000, 0, CD, 99, TH, false, true, Consent::Granted), Decision::Skip);
        assert_eq!(should_interject(200_000, 0, CD, 99, TH, false, false, Consent::Declined), Decision::Skip);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p mur-agent-runtime watch_scheduler::gate_tests`
Expected: PASS (5 tests).

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/src/watch_scheduler.rs
git commit -m "feat(watch): pure should_interject gate (mute/quiet/cooldown/consent)"
```

---

## Phase 3 — Runtime capture loop + wiring (build-only + manual E2E)

### Task 8: VLC snapshot capture + `WatchScheduler` loop

**Files:**
- Modify: `mur-agent-runtime/src/watch_scheduler.rs`

- [ ] **Step 1: Add capture + the scheduler (build-only — network/loop verified in E2E)**

Append to `mur-agent-runtime/src/watch_scheduler.rs`:

```rust
use crate::companion::schedule::active_window_end_for_today;
use crate::task_runner::{TaskRunner, TaskSpec};
use chrono::Local;
use mur_common::a2a::{Message, MessagePart};
use mur_common::agent::QuietHours;
use mur_common::media::{load_watch, runtime_path, save_watch, VlcRuntime};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

const DEFAULT_POLL_SECS: u64 = 6;
const SCENE_CHANGE_THRESHOLD: u32 = 18;
const INTERJECTION_COOLDOWN_MS: i64 = 45_000;
const NARRATE_PROMPT: &str = "畫面剛切換了，看一下螢幕，用一句話簡短說你看到什麼。";
const CONSENT_PROMPT: &str =
    "我可以在劇情轉折時，偶爾插一句話幫你補充嗎？不想聽的話跟我說「噓」就好。";

pub struct WatchScheduler {
    runner: Arc<TaskRunner>,
    mur_home: PathBuf,
    quiet_hours: Option<QuietHours>,
    tick: Duration,
}

impl WatchScheduler {
    pub fn new(runner: Arc<TaskRunner>, mur_home: PathBuf, quiet_hours: Option<QuietHours>) -> Self {
        Self { runner, mur_home, quiet_hours, tick: Duration::from_secs(DEFAULT_POLL_SECS) }
    }

    pub fn with_tick(mut self, d: Duration) -> Self {
        self.tick = d;
        self
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let _g = cancel.clone().drop_guard();
            run_loop(self, cancel).await;
        })
    }
}

/// Read VLC runtime config, if present.
fn vlc_runtime(mur_home: &std::path::Path) -> Option<VlcRuntime> {
    let body = std::fs::read_to_string(runtime_path(mur_home)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Newest regular file in `dir`, if any.
fn newest_file(dir: &std::path::Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let m = e.metadata().ok()?.modified().ok()?;
        if best.as_ref().map(|(t, _)| m > *t).unwrap_or(true) {
            best = Some((m, p));
        }
    }
    best.map(|(_, p)| p)
}

/// Ask VLC for a snapshot, read the resulting PNG, delete it, and return its bytes.
async fn capture_png(rt: &VlcRuntime, client: &reqwest::Client) -> Option<Vec<u8>> {
    let base = format!("http://127.0.0.1:{}/requests/status.xml", rt.port);
    // Is it playing? (cheap substring check; we don't need the full parser here.)
    let status = client
        .get(&base)
        .basic_auth("", Some(&rt.password))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    if !status.contains("<state>playing</state>") {
        return None;
    }
    // Trigger a snapshot.
    let _ = client
        .get(format!("{base}?command=snapshot"))
        .basic_auth("", Some(&rt.password))
        .timeout(Duration::from_secs(3))
        .send()
        .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let path = newest_file(&rt.snapshot_dir)?;
    let bytes = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path); // lifecycle: never accumulate (spec §6.3)
    Some(bytes)
}

/// Decode PNG bytes → 9×8 grayscale → dHash.
fn dhash_png(bytes: &[u8]) -> Option<u64> {
    use image::imageops::FilterType;
    let img = image::load_from_memory(bytes).ok()?;
    let small = img.grayscale().resize_exact(9, 8, FilterType::Triangle);
    let luma = small.to_luma8();
    Some(dhash_from_luma(luma.as_raw(), 9, 8))
}

fn inject(runner: &Arc<TaskRunner>, text: &str) {
    let runner = runner.clone();
    let input = Message { role: "user".into(), parts: vec![MessagePart::Text { text: text.into() }] };
    tokio::spawn(async move {
        let _ = runner.run_sync(TaskSpec { input, context_task_id: None }).await;
    });
}

async fn run_loop(s: WatchScheduler, cancel: CancellationToken) {
    let client = reqwest::Client::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(s.tick) => {}
        }
        let mut session = load_watch(&s.mur_home);
        if !session.active {
            continue;
        }
        let Some(rt) = vlc_runtime(&s.mur_home) else { continue };
        let Some(png) = capture_png(&rt, &client).await else { continue };
        let Some(hash) = dhash_png(&png) else { continue };

        let now_ms = Local::now().timestamp_millis();
        let quiet_now = active_window_end_for_today(Local::now(), s.quiet_hours.as_ref())
            .map(|dt| now_ms >= dt.timestamp_millis())
            .unwrap_or(false);
        let distance = hamming(hash, session.last_scene_phash);

        match should_interject(
            now_ms,
            session.last_interjection_ms,
            INTERJECTION_COOLDOWN_MS,
            distance,
            SCENE_CHANGE_THRESHOLD,
            session.muted,
            quiet_now,
            session.consent,
        ) {
            Decision::Narrate => {
                info!(distance, "watch: narrating scene change");
                inject(&s.runner, NARRATE_PROMPT);
                session.last_interjection_ms = now_ms;
            }
            Decision::AskConsent => {
                info!("watch: asking consent");
                inject(&s.runner, CONSENT_PROMPT);
                session.consent = Consent::Granted; // proceed next time; "噓" mutes.
                session.last_interjection_ms = now_ms;
            }
            Decision::Skip => {}
        }
        session.last_scene_phash = hash; // always track the latest frame
        let _ = save_watch(&s.mur_home, &session);
    }
}
```

- [ ] **Step 2: Verify the crate builds**

Run: `cargo build -p mur-agent-runtime`
Expected: builds. (If `active_window_end_for_today` is not `pub(crate)`-visible from this
module, make it `pub(crate)` in `mur-agent-runtime/src/companion/schedule.rs` — it is
already used by `idle_scheduler.rs`, so it is reachable.)

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/src/watch_scheduler.rs
git commit -m "feat(watch): VLC snapshot capture + WatchScheduler loop"
```

---

### Task 9: Spawn `WatchScheduler` in the supervisor

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs`

- [ ] **Step 1: Add the import**

Near the top of `mur-agent-runtime/src/supervisor.rs`, beside
`use crate::idle_scheduler::IdleScheduler;`, add:

```rust
use crate::watch_scheduler::WatchScheduler;
```

- [ ] **Step 2: Spawn it next to IdleScheduler**

Immediately after the IdleScheduler block (the `if !profile.inner.lifecycle.idle_triggers
.is_empty() { … transport_tasks.push(is.spawn()); … }` block, ~line 497), add:

```rust
    // 8d-bis. Proactive co-watching scheduler (spec §6). Cheap when no session is
    //         active (one file read per tick); only acts while watch.json is active.
    {
        let ws = WatchScheduler::new(
            runner.clone(),
            mur_home.to_path_buf(),
            profile.inner.companion.proactive.quiet_hours.clone(),
        );
        transport_tasks.push(ws.spawn());
        info!("WatchScheduler started");
    }
```

> If `mur_home` is not directly in scope at this point, thread it in: it is the
> `PathBuf` resolved at supervisor start (`supervisor.rs:92`) and already passed to
> sibling helpers (e.g. `supervisor.rs:280`, and the `mur_home: &Path` param at ~688).

- [ ] **Step 3: Build the runtime**

Run: `cargo build -p mur-agent-runtime`
Expected: builds.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs
git commit -m "feat(watch): spawn WatchScheduler from the supervisor"
```

---

### Task 10: Full verification + manual E2E

**Files:** none (verification only)

- [ ] **Step 1: Workspace build**

Run: `cargo build --workspace`
Expected: builds (`mur-agent-gui` is workspace-excluded).

- [ ] **Step 2: Targeted tests**

Run: `cargo test -p mur-common media:: ; cargo test -p mur-core media::watch ; cargo test -p mur-agent-runtime watch_scheduler::`
Expected: all PASS (mur-common media types + roundtrips; mur-core watch mutators;
runtime hash + gate tests).

- [ ] **Step 3: Lint + format**

Run: `cargo clippy -p mur-common -p mur-core -p mur-agent-runtime -p mur-mcp-server -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --check`
Expected: clean (run `cargo fmt` and amend if not).

- [ ] **Step 4: Manual E2E (requires MuR running as an agent runtime + VLC)**

1. Run MuR as a murmur agent runtime (e.g. the Hub concierge) so a `TaskRunner` +
   delivery channel exist.
2. `vlc_open` a non-DRM video and let it play.
3. Call `watch_start`. On the first big scene cut, expect a **consent question**
   delivered via the agent's channel; thereafter expect occasional one-line scene
   comments — never more often than the cooldown (~45s).
4. Verify `~/.mur/runtime/watch.json` updates (`last_scene_phash`,
   `last_interjection_ms`, `consent: granted`) and the snapshot dir does **not** grow
   (frames are deleted after hashing).
5. Say "噓" (or call `watch_mute`) → comments stop; `watch_status` shows `muted: true`.
6. `watch_stop` → no further activity; the loop idles cheaply.
7. Sanity: with no session active (`watch_stop`), confirm the scheduler causes no VLC
   snapshots (no new files appear in the snapshot dir).

- [ ] **Step 5: Final commit (if fmt/clippy required changes)**

```bash
git add -A
git commit -m "chore(watch): clippy/fmt fixups for proactive co-watching"
```

---

## Spec Coverage Check (Plan B scope)

- Spec §3 push/pull + crate-layering constraint → Tasks 1 (move types), 6–9 (runtime
  scheduler injects text turns; MCP tools are flags).
- Spec §6.1 `WatchSession`/`Consent` in mur-common → Task 2.
- Spec §6.2 `WatchScheduler` (snapshot → dHash → gate → inject) → Tasks 6–8.
- Spec §6.3 snapshot lifecycle (delete after hashing, never accumulate) → Task 8
  (`capture_png` removes the file).
- Spec §6.4 mute/"噓", `watch_*` tools (flags only), skill, `image` dep on runtime,
  VlcRuntime → mur-common → Tasks 1, 3, 4, 5, 6.
- Spec §9 Plan B file list → all covered.
- Deferred per spec §7 (NOT in this plan): idle auto-pause, transcript-window grounding
  of interjections, embedding RAG. The consent model is simplified to Unasked→Granted
  with "噓"→mute (Declined reserved); deepening consent capture is a follow-up.
```
