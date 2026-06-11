# Ambient Capture & Harvest (W1+W2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Flip `mur in`/`mur out` from "press record first" to "always recording, harvest afterwards" — hooks write per-session recordings ambiently; `mur out` becomes a review inbox of heuristically-gated workflow proposals.

**Architecture:** W1 promotes the existing ambient hook channel (`mur hook prompt|tool|stop`) to also write `~/.mur/session/recordings/<session_id>.jsonl` (per-session keying, scrub-at-write, retention GC); `mur in` becomes an importance marker. W2 adds a zero-token harvest pipeline: heuristic gate → skeletonized step proposals in `~/.mur/inbox/workflow-proposals/` → `mur out` interactive review → draft `Workflow` via the existing `WorkflowYamlStore`. No LLM calls are added in W1/W2 (the existing `mur out --action analyze` LLM path is untouched).

**Tech Stack:** Rust edition 2024, serde/serde_yaml/serde_json, clap (derive), dialoguer, chrono, anyhow. Tests run with `cargo nextest run -p mur-core` (NOT plain `cargo test --workspace` — 7 unrelated tests fail spuriously outside nextest).

**Spec:** `docs/superpowers/specs/2026-06-11-mur-ambient-capture-and-harvest-design.md` (§3.1–3.3, §3.7 config fields, §3.8 tier-1 hint). W3–W5 are out of scope here (W3 = workflow-engine v2 spec's own plan; W4/W5 blocked on server/Hub Phase 3).

**Conventions used throughout:**
- All new thresholds live in config (Mandatory Rule #1).
- Files stay ≤ 800 lines (Mandatory Rule #4) — `cmd/session.rs` is at 1496 and `cmd/hook.rs` at ~500; new logic goes into NEW modules (`session/ambient.rs`, `harvest/*`), only thin wiring is added to existing files.
- Test helpers take an explicit directory parameter (`_in_dir` pattern, see `session::remove_recording_in_dir`) so tests never touch `~/.mur` and never race on `MUR_HOME`.
- Commit messages follow repo style: `feat(session): …`, `feat(harvest): …`.

---

## File Structure

**W1 (capture flip)**
- Modify: `mur-common/src/config.rs` — add `SessionCfg` + `HarvestCfg` sections
- Modify: `mur-core/src/session/mod.rs` — `SessionMeta` new fields; path helpers via `paths::mur_root`; low-level `record_event_in_dir`
- Modify: `mur-core/src/session/scrub.rs` — expose single-event scrub for write-time use
- Create: `mur-core/src/session/ambient.rs` — ambient capture (session keying, mark-next consumption, Layer-1 enrichment)
- Modify: `mur-core/src/inject/event.rs` — `NormalizedEvent` gains `transcript_path` / `tool_response` / `cwd` (claude parser fills them)
- Modify: `mur-core/src/cmd/hook.rs` — wire capture into prompt/tool/stop; spawn `mur session gc` from session-start
- Modify: `mur-core/src/cmd/session.rs` — `cmd_in` marker semantics; `cmd_session_gc`
- Modify: `mur-core/src/cli/mod.rs`, `mur-core/src/dispatch.rs` — `SessionAction::Gc`

**W2 (harvest inbox)**
- Create: `mur-core/src/harvest/mod.rs` — `scan()` orchestration
- Create: `mur-core/src/harvest/gate.rs` — heuristic gate (config thresholds; `marked` bypass)
- Create: `mur-core/src/harvest/skeleton.rs` — literal-stripping command skeletons
- Create: `mur-core/src/harvest/proposal.rs` — proposal YAML store + Jaccard near-dup
- Modify: `mur-core/src/lib.rs` — `pub mod harvest;`
- Modify: `mur-core/src/cmd/workflow.rs` — `create_draft_workflow_with_steps`
- Modify: `mur-core/src/cmd/session.rs` — `cmd_out` review flow
- Modify: `mur-core/src/cmd/hook.rs` — SessionStart pending-proposal hint
- Modify: `skills/mur-in/SKILL.md`, `skills/mur-out/SKILL.md` (if present in repo) — new semantics
- Modify: `docs/architecture/runtime-overview.md`, `README.md` — document the new flow

---

## W1 — Capture flip

### Task 1: Config sections `session:` and `harvest:`

**Files:**
- Modify: `mur-common/src/config.rs` (struct `Config` is at line 14)
- Test: same file, `#[cfg(test)]` section

- [ ] **Step 1: Check whether `default_true` helper already exists**

Run: `grep -n "fn default_true" /Volumes/Firecuda4tb/Projects/mur/mur-common/src/config.rs`
If it exists, reuse it in Step 3 and do NOT redefine it. If absent, include it as shown.

- [ ] **Step 2: Write the failing test**

Append inside the existing `#[cfg(test)] mod tests` at the bottom of `mur-common/src/config.rs` (create the mod if the file has none — check with `grep -n "mod tests" mur-common/src/config.rs`):

```rust
#[test]
fn session_and_harvest_defaults() {
    let cfg: Config = serde_yaml::from_str("{}").unwrap();
    assert_eq!(cfg.session.capture, "ambient");
    assert_eq!(cfg.session.retention_days, 14);
    assert!(cfg.harvest.auto_gate);
    assert_eq!(cfg.harvest.llm, "local-first");
    assert_eq!(cfg.harvest.min_events, 5);
    assert_eq!(cfg.harvest.min_user_turns, 2);
    assert_eq!(cfg.harvest.min_duration_secs, 120);
    assert_eq!(cfg.harvest.idle_minutes, 30);
    assert_eq!(cfg.harvest.max_llm_calls_per_day, 10);
    assert_eq!(cfg.harvest.max_extract_input_tokens, 12000);
    assert!(cfg.harvest.session_start_hint);
    assert!((cfg.harvest.similarity_merge_threshold - 0.6).abs() < f32::EPSILON);
}

#[test]
fn session_capture_override_parses() {
    let cfg: Config = serde_yaml::from_str("session:\n  capture: off\n  retention_days: 3\n").unwrap();
    assert_eq!(cfg.session.capture, "off");
    assert_eq!(cfg.session.retention_days, 3);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run -p mur-common session_and_harvest`
Expected: FAIL — `no field 'session' on type 'Config'` (compile error counts as the failing state).

- [ ] **Step 4: Implement the config structs**

In `mur-common/src/config.rs`, add two fields to `pub struct Config` (after the last existing section field, keeping the `// --- ... additions ---` comment style):

```rust
    // --- Ambient capture & harvest (2026-06-11 spec) ---
    #[serde(default)]
    pub session: SessionCfg,

    #[serde(default)]
    pub harvest: HarvestCfg,
```

Then add below the other section structs (e.g. after `NudgeConfig`):

```rust
/// Ambient session capture (spec 2026-06-11-mur-ambient-capture-and-harvest §3.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCfg {
    /// "ambient" (hooks always record) | "manual" (legacy `mur session in` gate) | "off"
    #[serde(default = "default_capture_mode")]
    pub capture: String,
    /// Recordings older than this many days are removed by `mur session gc`.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

impl Default for SessionCfg {
    fn default() -> Self {
        Self {
            capture: default_capture_mode(),
            retention_days: default_retention_days(),
        }
    }
}

fn default_capture_mode() -> String {
    "ambient".to_string()
}
fn default_retention_days() -> u32 {
    14
}

/// Harvest gate + token-budget defenses (spec §3.2, §3.7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarvestCfg {
    /// Run the heuristic gate automatically (from `mur session gc` / `mur out`).
    #[serde(default = "default_true")]
    pub auto_gate: bool,
    /// "local-first" | "cloud" | "off" — W1/W2 only persist this; LLM wiring lands with v2 P5a.
    #[serde(default = "default_harvest_llm")]
    pub llm: String,
    /// Gate thresholds — a session must clear at least one of these (see harvest::gate).
    #[serde(default = "default_min_events")]
    pub min_events: usize,
    #[serde(default = "default_min_user_turns")]
    pub min_user_turns: usize,
    #[serde(default = "default_min_duration_secs")]
    pub min_duration_secs: i64,
    /// A session is considered ended when its last event is older than this.
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: i64,
    /// §3.7 hard caps (persisted now; enforced when the LLM extract path lands in v2 P5a).
    #[serde(default = "default_max_llm_calls_per_day")]
    pub max_llm_calls_per_day: u32,
    #[serde(default = "default_max_extract_input_tokens")]
    pub max_extract_input_tokens: usize,
    /// §3.8 tier-1: one-line pending-proposals hint at SessionStart.
    #[serde(default = "default_true")]
    pub session_start_hint: bool,
    /// Step-skeleton Jaccard similarity at/above which a proposal becomes a merge suggestion.
    #[serde(default = "default_similarity_merge_threshold")]
    pub similarity_merge_threshold: f32,
}

impl Default for HarvestCfg {
    fn default() -> Self {
        serde_yaml::from_str("{}").expect("HarvestCfg defaults")
    }
}

fn default_harvest_llm() -> String {
    "local-first".to_string()
}
fn default_min_events() -> usize {
    5
}
fn default_min_user_turns() -> usize {
    2
}
fn default_min_duration_secs() -> i64 {
    120
}
fn default_idle_minutes() -> i64 {
    30
}
fn default_max_llm_calls_per_day() -> u32 {
    10
}
fn default_max_extract_input_tokens() -> usize {
    12000
}
fn default_similarity_merge_threshold() -> f32 {
    0.6
}
```

(If Step 1 found no `default_true`, also add: `fn default_true() -> bool { true }`.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p mur-common session_and_harvest`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(config): add session capture + harvest config sections"
```

---

### Task 2: `SessionMeta` new fields + `MUR_HOME`-aware paths + `record_event_in_dir`

**Files:**
- Modify: `mur-core/src/session/mod.rs` (whole file is 778 lines; key spots: `session_dir()` line 36, `SessionMeta` line 67, `record()` line 192)

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `mur-core/src/session/mod.rs`:

```rust
#[test]
fn meta_back_compat_without_new_fields() {
    // Old meta files (pre-ambient) must keep parsing.
    let json = r#"{"id":"x","source":"claude","started_at":"2026-01-01T00:00:00Z",
        "stopped_at":null,"title":null,"tools_used":[],"user_turns":0,"assistant_turns":0}"#;
    let meta: SessionMeta = serde_json::from_str(json).unwrap();
    assert!(!meta.marked);
    assert!(meta.gated_at.is_none());
    assert!(meta.harvested_at.is_none());
}

#[test]
fn record_event_in_dir_appends_and_updates_meta() {
    let tmp = tempfile::TempDir::new().unwrap();
    let rec_dir = tmp.path().join("recordings");
    fs::create_dir_all(&rec_dir).unwrap();

    let ev = SessionEvent {
        timestamp: 1000,
        event_type: "user".to_string(),
        tool: None,
        content: "fix the login bug".to_string(),
        working_dir: Some("/repo".to_string()),
        git_branch: Some("main".to_string()),
        exit_code: None,
    };
    record_event_in_dir(&rec_dir, "sess-1", "claude", &ev).unwrap();
    record_event_in_dir(&rec_dir, "sess-1", "claude", &ev).unwrap();

    let content = fs::read_to_string(rec_dir.join("sess-1.jsonl")).unwrap();
    assert_eq!(content.lines().count(), 2);

    let meta: SessionMeta =
        serde_json::from_str(&fs::read_to_string(rec_dir.join("sess-1.meta.json")).unwrap())
            .unwrap();
    assert_eq!(meta.user_turns, 2);
    assert_eq!(meta.source, "claude");
    assert_eq!(meta.title.as_deref(), Some("fix the login bug"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-core meta_back_compat record_event_in_dir`
Expected: FAIL to compile — `SessionEvent` has no `working_dir`, `record_event_in_dir` not found.

- [ ] **Step 3: Implement**

In `mur-core/src/session/mod.rs`:

3a. Extend `SessionEvent` (line 27) — all new fields optional so old recordings keep parsing:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub timestamp: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub content: String,
    // ── Layer-1 enrichment (spec §3.1; all Option/default for back-compat) ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}
```

> Every existing construction site of `SessionEvent` in the crate now needs the three new fields. Run `grep -rn "SessionEvent {" mur-core/src/` and add `working_dir: None, git_branch: None, exit_code: None,` to each literal (including the tests in this file and `session/scrub.rs::scrub_events`, which clones field-by-field — add the three clones there).

3b. Extend `SessionMeta` (line 67):

```rust
    pub user_turns: usize,
    pub assistant_turns: usize,
    // ── Ambient capture additions (all default for back-compat) ──
    /// Set by `mur in`: harvest gate passes this session unconditionally.
    #[serde(default)]
    pub marked: bool,
    /// RFC3339 of the last harvest-gate run over this session (prevents re-scan).
    #[serde(default)]
    pub gated_at: Option<String>,
    /// RFC3339 when the user accepted/skipped this session's proposal.
    #[serde(default)]
    pub harvested_at: Option<String>,
```

> Same sweep: `grep -rn "SessionMeta {" mur-core/src/` and add `marked: false, gated_at: None, harvested_at: None,` to every literal (`start()` line 155, tests, and any other construction site the grep finds).

3c. Make the path helpers `MUR_HOME`-aware (replaces `session_dir()` line 36):

```rust
fn session_dir() -> PathBuf {
    crate::paths::mur_root(None).join("session")
}
```

(`recordings_dir()`/`active_path()` already derive from `session_dir()` — no other change.)

3d. Add the low-level writer (place directly above `record()` at line 192). It is the extracted body of `record()` so both the legacy path and ambient capture share one implementation:

```rust
/// Append one event to `<recordings_dir>/<id>.jsonl`, creating the meta file on
/// first write. Shared by the legacy active-session path and ambient capture.
pub(crate) fn record_event_in_dir(
    recordings_dir: &std::path::Path,
    id: &str,
    source: &str,
    event: &SessionEvent,
) -> Result<()> {
    fs::create_dir_all(recordings_dir)?;

    let recording_path = recordings_dir.join(format!("{}.jsonl", id));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&recording_path)
        .context("Failed to open recording file")?;

    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;

    // Load-or-create meta, then apply the same turn/tool accounting as before.
    let meta_path = recordings_dir.join(format!("{}.meta.json", id));
    let mut meta: SessionMeta = fs::read_to_string(&meta_path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_else(|| SessionMeta {
            id: id.to_string(),
            source: source.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            stopped_at: None,
            title: None,
            tools_used: vec![],
            user_turns: 0,
            assistant_turns: 0,
            marked: false,
            gated_at: None,
            harvested_at: None,
        });

    match event.event_type.as_str() {
        "user" => {
            meta.user_turns += 1;
            if meta.title.is_none() {
                let title: String = event.content.chars().take(80).collect();
                meta.title = Some(title);
            }
        }
        "assistant" => meta.assistant_turns += 1,
        "tool_call" => {
            if let Some(tool_name) = &event.tool {
                let tools: BTreeSet<String> = meta.tools_used.iter().cloned().collect();
                if !tools.contains(tool_name) {
                    meta.tools_used.push(tool_name.clone());
                }
            }
        }
        _ => {}
    }
    let json = serde_json::to_string_pretty(&meta)?;
    fs::write(&meta_path, json).context("Failed to write session meta")?;
    Ok(())
}
```

3e. Rewrite the body of `record()` (line 192) to delegate (public contract unchanged):

```rust
pub fn record(event_type: &str, tool: Option<&str>, content: &str) -> Result<bool> {
    let active = active_path();
    if !active.exists() {
        return Ok(false);
    }
    if should_skip(event_type, content) {
        return Ok(true);
    }
    let session_content = fs::read_to_string(&active)?;
    let session: ActiveSession = serde_json::from_str(&session_content)?;

    let event = SessionEvent {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        event_type: event_type.to_string(),
        tool: tool.map(|s| s.to_string()),
        content: content.to_string(),
        working_dir: None,
        git_branch: None,
        exit_code: None,
    };
    record_event_in_dir(&recordings_dir(), &session.id, &session.source, &event)?;
    Ok(true)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core -E 'binary(mur-core)' session::`
Expected: PASS, including all pre-existing `session::tests`.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/session/mod.rs mur-core/src/session/scrub.rs
git commit -m "feat(session): Layer-1 event fields, meta marked/gated/harvested, shared record_event_in_dir"
```

---

### Task 3: `NormalizedEvent` enrichment (transcript_path, tool_response, cwd)

**Files:**
- Modify: `mur-core/src/inject/event.rs` (struct at line 15, `parse_claude` at line 45; four other parsers below)

- [ ] **Step 1: Write the failing test**

Append to the test module of `inject/event.rs` (check `grep -n "mod tests" mur-core/src/inject/event.rs`; create one at the bottom if absent):

```rust
#[test]
fn parse_claude_captures_enrichment_fields() {
    let raw = serde_json::json!({
        "session_id": "s1",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/repo",
        "tool_name": "Bash",
        "tool_input": {"command": "cargo build"},
        "tool_response": {"stdout": "ok", "exit_code": 0}
    });
    let ev = parse_event(raw, EventKind::Tool, "claude");
    assert_eq!(ev.transcript_path.as_deref(), Some("/tmp/t.jsonl"));
    assert_eq!(ev.cwd.as_deref(), Some("/repo"));
    assert_eq!(ev.tool_response.as_ref().unwrap()["exit_code"], 0);
}

#[test]
fn old_queue_lines_still_deserialize() {
    // Queue lines written before this change lack the new fields.
    let line = r#"{"kind":"tool","tool_provider":"claude","query":null,"tool_called":"Bash",
        "tool_input":null,"stop_reason":null,"session_id":"s1"}"#;
    let ev: NormalizedEvent = serde_json::from_str(line).unwrap();
    assert!(ev.transcript_path.is_none());
    assert!(ev.tool_response.is_none());
    assert!(ev.cwd.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-core parse_claude_captures old_queue_lines`
Expected: FAIL to compile — no field `transcript_path`.

- [ ] **Step 3: Implement**

3a. Add to `NormalizedEvent` (after `session_id`):

```rust
    /// Claude Code Stop payloads carry the transcript path; used to recover the
    /// last assistant message for ambient capture (spec §3.1).
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// PostToolUse result payload (claude); exit_code is extracted from it.
    #[serde(default)]
    pub tool_response: Option<Value>,
    /// Hook-reported working directory.
    #[serde(default)]
    pub cwd: Option<String>,
```

3b. In `parse_claude`, add before `duration_ms: None`:

```rust
        transcript_path: raw
            .get("transcript_path")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        tool_response: raw.get("tool_response").cloned(),
        cwd: raw.get("cwd").and_then(|v| v.as_str()).map(str::to_owned),
```

3c. In `parse_gemini`, `parse_cursor`, `parse_copilot`, `parse_opencode` add (before `duration_ms: None` in each):

```rust
        transcript_path: None,
        tool_response: None,
        cwd: None,
```

> Sweep any other `NormalizedEvent {` literal: `grep -rn "NormalizedEvent {" mur-core/src/` — add the three `None` fields to each (hook.rs builds duration records; stats tests build fixtures).

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core inject::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/inject/event.rs mur-core/src/cmd/hook.rs
git commit -m "feat(inject): capture transcript_path/tool_response/cwd from claude hook payloads"
```

---

### Task 4: Write-time scrub helper

**Files:**
- Modify: `mur-core/src/session/scrub.rs` (module header says "Only applied before cloud push" — update it)

- [ ] **Step 1: Write the failing test**

Append to scrub.rs tests:

```rust
#[test]
fn scrub_event_redacts_in_place() {
    let ev = SessionEvent {
        timestamp: 1,
        event_type: "tool_call".to_string(),
        tool: Some("Bash".to_string()),
        content: "export GITHUB_TOKEN=ghp_0123456789abcdefghijklmnopqrstuvwxyz0123".to_string(),
        working_dir: None,
        git_branch: None,
        exit_code: None,
    };
    let scrubbed = scrub_event(&ev);
    assert!(!scrubbed.content.contains("ghp_"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core scrub_event_redacts`
Expected: FAIL to compile — `scrub_event` not found.

- [ ] **Step 3: Implement**

Add below `scrub_events` in scrub.rs, reusing the existing private `scrub_content`:

```rust
/// Scrub a single event at write time (ambient capture path).
pub fn scrub_event(e: &SessionEvent) -> SessionEvent {
    SessionEvent {
        timestamp: e.timestamp,
        event_type: e.event_type.clone(),
        tool: e.tool.clone(),
        content: scrub_content(&e.content, &e.event_type),
        working_dir: e.working_dir.clone(),
        git_branch: e.git_branch.clone(),
        exit_code: e.exit_code,
    }
}
```

Update the module doc comment line `//! Only applied before cloud upload — local .jsonl files are never modified.` to:

```rust
//! Applied (a) before cloud upload and (b) at write time on the ambient
//! capture path, so secrets never reach disk in new recordings.
```

- [ ] **Step 4: Run tests** — `cargo nextest run -p mur-core scrub` → PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/session/scrub.rs
git commit -m "feat(session): single-event scrub for write-time ambient capture"
```

---

### Task 5: `session/ambient.rs` — the capture core

**Files:**
- Create: `mur-core/src/session/ambient.rs`
- Modify: `mur-core/src/session/mod.rs` — add `pub mod ambient;` after `pub mod cloud;` (line 7)

- [ ] **Step 1: Create the module with failing tests**

Write `mur-core/src/session/ambient.rs`:

```rust
//! Ambient session capture (spec 2026-06-11 §3.1).
//!
//! Hooks call [`capture`] on every prompt / post-tool / stop event. Events are
//! written to `~/.mur/session/recordings/<session_key>.jsonl` keyed by the hook
//! payload's session_id — no `active.json` gate, no agent compliance needed.
//! Secrets are scrubbed at write time; noise is filtered with `should_skip`.

use anyhow::Result;
use std::path::Path;

use super::{SessionEvent, should_skip};
use crate::inject::event::{EventKind, NormalizedEvent};

/// Marker file written by `mur in`: the next captured event marks its session.
pub(crate) const MARK_NEXT_FILE: &str = "mark-next";

/// Resolve the recording key for an event. Providers without session ids
/// (cursor, copilot) fall into a per-provider daily bucket.
pub fn session_key(ev: &NormalizedEvent) -> String {
    match ev.session_id.as_deref() {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => format!(
            "{}-ambient-{}",
            ev.tool_provider,
            chrono::Utc::now().format("%Y-%m-%d")
        ),
    }
}

/// Best-effort exit code from a claude PostToolUse `tool_response`.
fn exit_code_of(ev: &NormalizedEvent) -> Option<i32> {
    let resp = ev.tool_response.as_ref()?;
    resp.get("exit_code")
        .or_else(|| resp.get("exitCode"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
}

/// Read the current git branch from `<cwd>/.git/HEAD` without spawning git.
fn git_branch_of(cwd: Option<&str>) -> Option<String> {
    let head = Path::new(cwd?).join(".git").join("HEAD");
    let content = std::fs::read_to_string(head).ok()?;
    content
        .trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_owned)
}

/// Map a hook event to a recordable SessionEvent. Returns None for kinds that
/// carry nothing recordable (e.g. SessionStart, empty prompts).
pub fn to_session_event(ev: &NormalizedEvent) -> Option<SessionEvent> {
    let (event_type, tool, content) = match ev.kind {
        EventKind::Prompt => {
            let q = ev.query.as_deref().unwrap_or("").trim();
            if q.is_empty() {
                return None;
            }
            ("user", None, q.to_string())
        }
        EventKind::Tool => {
            let name = ev.tool_called.clone()?;
            let input = ev
                .tool_input
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default();
            ("tool_call", Some(name), input)
        }
        EventKind::Stop => {
            let content = last_assistant_message(ev.transcript_path.as_deref())?;
            ("assistant", None, content)
        }
        EventKind::SessionStart => return None,
    };
    Some(SessionEvent {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        event_type: event_type.to_string(),
        tool,
        content,
        working_dir: ev.cwd.clone(),
        git_branch: git_branch_of(ev.cwd.as_deref()),
        exit_code: exit_code_of(ev),
    })
}

/// Last assistant text from a Claude Code transcript JSONL, capped to
/// `ASSISTANT_CAP_CHARS` so recordings stay small.
const ASSISTANT_CAP_CHARS: usize = 2000;

fn last_assistant_message(transcript_path: Option<&str>) -> Option<String> {
    let content = std::fs::read_to_string(transcript_path?).ok()?;
    for line in content.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        // transcript schema: {type:"assistant", message:{content:[{type:"text",text:"…"}, …]}}
        let texts: Vec<String> = v
            .pointer("/message/content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| {
                        (b.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .then(|| b.get("text").and_then(|t| t.as_str()).unwrap_or(""))
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let joined = texts.join("\n");
        if !joined.trim().is_empty() {
            return Some(joined.chars().take(ASSISTANT_CAP_CHARS).collect());
        }
    }
    None
}

/// Capture into an explicit session dir (testable core). Returns true if an
/// event was written.
pub fn capture_in_dir(session_dir: &Path, ev: &NormalizedEvent) -> Result<bool> {
    let Some(mut event) = to_session_event(ev) else {
        return Ok(false);
    };
    if should_skip(&event.event_type, &event.content) {
        return Ok(false);
    }
    event = super::scrub::scrub_event(&event);

    let recordings = session_dir.join("recordings");
    let key = session_key(ev);
    super::record_event_in_dir(&recordings, &key, &ev.tool_provider, &event)?;

    // `mur in` wrote a mark-next marker: mark this session and consume it.
    let marker = session_dir.join(MARK_NEXT_FILE);
    if marker.exists() {
        let meta_path = recordings.join(format!("{}.meta.json", key));
        if let Ok(content) = std::fs::read_to_string(&meta_path)
            && let Ok(mut meta) = serde_json::from_str::<super::SessionMeta>(&content)
        {
            meta.marked = true;
            if let Ok(json) = serde_json::to_string_pretty(&meta) {
                let _ = std::fs::write(&meta_path, json);
            }
            let _ = std::fs::remove_file(&marker);
        }
    }
    Ok(true)
}

/// Hook entry point: honors `session.capture` config; never panics, never
/// blocks the hook on error (callers `let _ =` the result).
pub fn capture(ev: &NormalizedEvent) -> Result<bool> {
    let mode = crate::store::config::load_config()
        .map(|c| c.session.capture)
        .unwrap_or_else(|_| "ambient".to_string());
    if mode != "ambient" {
        // "manual" keeps the legacy active.json path; "off" records nothing.
        return Ok(false);
    }
    capture_in_dir(&crate::paths::mur_root(None).join("session"), ev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inject::event::{EventKind, NormalizedEvent};

    fn ev(kind: EventKind) -> NormalizedEvent {
        NormalizedEvent {
            kind,
            tool_provider: "claude".to_string(),
            query: Some("deploy the api".to_string()),
            tool_called: Some("Bash".to_string()),
            tool_input: Some(serde_json::json!({"command": "fly deploy"})),
            stop_reason: None,
            session_id: Some("sess-amb-1".to_string()),
            transcript_path: None,
            tool_response: Some(serde_json::json!({"exit_code": 0})),
            cwd: None,
            duration_ms: None,
            is_duration_record: false,
        }
    }

    #[test]
    fn capture_prompt_and_tool_writes_keyed_recording() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sdir = tmp.path().join("session");

        assert!(capture_in_dir(&sdir, &ev(EventKind::Prompt)).unwrap());
        assert!(capture_in_dir(&sdir, &ev(EventKind::Tool)).unwrap());

        let rec = sdir.join("recordings").join("sess-amb-1.jsonl");
        let content = std::fs::read_to_string(&rec).unwrap();
        assert_eq!(content.lines().count(), 2);
        let last: crate::session::SessionEvent =
            serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert_eq!(last.event_type, "tool_call");
        assert_eq!(last.exit_code, Some(0));
    }

    #[test]
    fn session_key_falls_back_to_daily_bucket() {
        let mut e = ev(EventKind::Prompt);
        e.session_id = None;
        e.tool_provider = "cursor".to_string();
        let key = session_key(&e);
        assert!(key.starts_with("cursor-ambient-"));
    }

    #[test]
    fn mark_next_marker_marks_and_is_consumed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sdir = tmp.path().join("session");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(sdir.join(MARK_NEXT_FILE), "").unwrap();

        capture_in_dir(&sdir, &ev(EventKind::Prompt)).unwrap();

        let meta: crate::session::SessionMeta = serde_json::from_str(
            &std::fs::read_to_string(
                sdir.join("recordings").join("sess-amb-1.meta.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(meta.marked);
        assert!(!sdir.join(MARK_NEXT_FILE).exists());
    }

    #[test]
    fn stop_without_transcript_records_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sdir = tmp.path().join("session");
        let mut e = ev(EventKind::Stop);
        e.transcript_path = None;
        assert!(!capture_in_dir(&sdir, &e).unwrap());
    }

    #[test]
    fn stop_reads_last_assistant_from_transcript() {
        let tmp = tempfile::TempDir::new().unwrap();
        let transcript = tmp.path().join("t.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done: deployed"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let sdir = tmp.path().join("session");
        let mut e = ev(EventKind::Stop);
        e.transcript_path = Some(transcript.to_string_lossy().to_string());
        assert!(capture_in_dir(&sdir, &e).unwrap());
        let content =
            std::fs::read_to_string(sdir.join("recordings").join("sess-amb-1.jsonl")).unwrap();
        assert!(content.contains("done: deployed"));
    }
}
```

Register the module in `mur-core/src/session/mod.rs` line 7:

```rust
pub mod ambient;
pub mod cloud;
pub mod scrub;
```

- [ ] **Step 2: Run tests to verify current state**

Run: `cargo nextest run -p mur-core ambient::`
Expected: PASS (the module ships with its tests; if anything fails, fix before committing — likely a missed field sweep from Tasks 2–3).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/session/ambient.rs mur-core/src/session/mod.rs
git commit -m "feat(session): ambient capture core — keyed recordings, mark-next, transcript tail"
```

---

### Task 6: Wire ambient capture into the hooks

**Files:**
- Modify: `mur-core/src/cmd/hook.rs` — `cmd_hook_prompt` (line 100), `cmd_hook_tool` (line 182), `cmd_hook_stop` (line 254), `cmd_hook_session_start` (line 262)

- [ ] **Step 1: Wire prompt capture**

In `cmd_hook_prompt`, directly after `let _ = enqueue(&event);` (line 104), add:

```rust
    let _ = crate::session::ambient::capture(&event);
```

- [ ] **Step 2: Wire post-tool capture**

In `cmd_hook_tool`, the function currently returns early for non-PreToolUse payloads (lines ~197-199):

```rust
    if !is_pre_tool_use(&raw) {
        return Ok(());
    }
```

Change to capture PostToolUse before returning:

```rust
    if !is_pre_tool_use(&raw) {
        // PostToolUse: ambient-capture the executed tool call (input + exit code).
        let _ = crate::session::ambient::capture(&event);
        return Ok(());
    }
```

- [ ] **Step 3: Wire stop capture + background gc**

In `cmd_hook_stop` (line 254), after `let _ = enqueue(&event);`:

```rust
    let _ = crate::session::ambient::capture(&event);
```

In `cmd_hook_session_start` (line 262), after `let _ = enqueue(&event);`, add a detached gc spawn (same pattern as `spawn_background_pipeline`):

```rust
    // Housekeeping: retention GC (+ harvest scan from W2) runs detached so the
    // hook stays fast.
    let mur_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("mur"));
    let _ = std::process::Command::new(&mur_bin)
        .args(["session", "gc"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
```

- [ ] **Step 4: Build + clippy**

Run: `cargo clippy -p mur-core -- -D warnings`
Expected: clean. (`mur session gc` doesn't exist yet — that's Task 7; the spawn just no-ops with a CLI error in the detached child until then, which is acceptable within the same PR. Do Task 7 before pushing.)

- [ ] **Step 5: Manual smoke test**

```bash
MUR_HOME=$(mktemp -d) sh -c '
  echo "{\"session_id\":\"smoke-1\",\"prompt\":\"hello ambient\",\"cwd\":\"/tmp\"}" \
    | cargo run -q -p mur-core --bin mur -- hook prompt --tool claude
  cat "$MUR_HOME/session/recordings/smoke-1.jsonl"
'
```
Expected: one JSON line with `"type":"user"` and `"content":"hello ambient"`.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/hook.rs
git commit -m "feat(hook): ambient-capture prompt/post-tool/stop events; spawn session gc at session-start"
```

---

### Task 7: `mur session gc` (retention) + `mur in` marker semantics

**Files:**
- Modify: `mur-core/src/cli/mod.rs` — `SessionAction` enum (search `pub enum SessionAction`)
- Modify: `mur-core/src/dispatch.rs` — `SessionAction` match (line ~197-230)
- Modify: `mur-core/src/cmd/session.rs` — new `cmd_session_gc`; rewrite `cmd_in` (line 291)
- Modify: `mur-core/src/session/mod.rs` — `gc_in_dir` helper

- [ ] **Step 1: Write the failing test for gc**

Append to `mod tests` in `mur-core/src/session/mod.rs`:

```rust
#[test]
fn gc_removes_old_unmarked_keeps_recent_and_marked() {
    let tmp = tempfile::TempDir::new().unwrap();
    let rec_dir = tmp.path().join("recordings");
    fs::create_dir_all(&rec_dir).unwrap();

    let write = |id: &str, started_days_ago: i64, marked: bool, harvested: bool| {
        fs::write(rec_dir.join(format!("{}.jsonl", id)), "{}\n").unwrap();
        let meta = SessionMeta {
            id: id.to_string(),
            source: "claude".to_string(),
            started_at: (chrono::Utc::now() - chrono::Duration::days(started_days_ago))
                .to_rfc3339(),
            stopped_at: None,
            title: None,
            tools_used: vec![],
            user_turns: 0,
            assistant_turns: 0,
            marked,
            gated_at: None,
            harvested_at: harvested.then(|| chrono::Utc::now().to_rfc3339()),
        };
        fs::write(
            rec_dir.join(format!("{}.meta.json", id)),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    };

    write("old-plain", 30, false, false); // old, unmarked → removed
    write("old-marked", 30, true, false); // old but marked, never harvested → kept
    write("old-marked-done", 30, true, true); // old, marked, harvested → removed
    write("fresh", 1, false, false); // recent → kept

    let removed = gc_in_dir(&rec_dir, 14).unwrap();
    assert_eq!(removed, 2);
    assert!(!rec_dir.join("old-plain.jsonl").exists());
    assert!(rec_dir.join("old-marked.jsonl").exists());
    assert!(!rec_dir.join("old-marked-done.jsonl").exists());
    assert!(rec_dir.join("fresh.jsonl").exists());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core gc_removes_old`
Expected: FAIL to compile — `gc_in_dir` not found.

- [ ] **Step 3: Implement `gc_in_dir`**

Add to `mur-core/src/session/mod.rs` (below `remove_recording`):

```rust
/// Remove recordings older than `retention_days`, judged by meta `started_at`.
/// Marked-but-never-harvested sessions are kept regardless of age (the user
/// declared them important; they leave via harvest or explicit remove).
/// Returns the number of recordings removed.
pub fn gc_in_dir(recordings_dir: &std::path::Path, retention_days: u32) -> Result<usize> {
    if !recordings_dir.exists() {
        return Ok(0);
    }
    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
    let mut removed = 0usize;
    for entry in fs::read_dir(recordings_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let meta = fs::read_to_string(recordings_dir.join(format!("{}.meta.json", id)))
            .ok()
            .and_then(|c| serde_json::from_str::<SessionMeta>(&c).ok());
        let Some(meta) = meta else { continue }; // metaless files: leave for manual cleanup
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&meta.started_at) else {
            continue;
        };
        if started.with_timezone(&chrono::Utc) >= cutoff {
            continue;
        }
        if meta.marked && meta.harvested_at.is_none() {
            continue;
        }
        remove_recording_in_dir(recordings_dir, &id)?;
        removed += 1;
    }
    Ok(removed)
}
```

- [ ] **Step 4: Run test** — `cargo nextest run -p mur-core gc_removes_old` → PASS.

- [ ] **Step 5: Add the CLI verb + dispatch + command**

5a. `mur-core/src/cli/mod.rs`, inside `pub enum SessionAction` (after the `Remove` variant — search `enum SessionAction`):

```rust
    /// Remove recordings past retention and run harvest housekeeping
    #[command(hide = true)]
    Gc,
```

5b. `mur-core/src/dispatch.rs`, in the `SessionAction` match (the arm block starting at line ~197):

```rust
            SessionAction::Gc => cmd::session::cmd_session_gc()?,
```

5c. `mur-core/src/cmd/session.rs`, add near `cmd_session_record` (line 614):

```rust
/// Retention GC over ambient recordings. Quiet by design — runs detached from
/// the session-start hook. Harvest scan is appended here in W2.
pub(crate) fn cmd_session_gc() -> Result<()> {
    let cfg = crate::store::config::load_config()?;
    let recordings = crate::paths::mur_root(None)
        .join("session")
        .join("recordings");
    let removed = crate::session::gc_in_dir(&recordings, cfg.session.retention_days)?;
    if removed > 0 {
        eprintln!("session gc: removed {} expired recording(s)", removed);
    }
    Ok(())
}
```

- [ ] **Step 6: Rewrite `cmd_in` (marker semantics)**

Replace the body of `cmd_in` in `mur-core/src/cmd/session.rs` (line 291). Current body calls `crate::session::start(source)`. New body:

```rust
pub(crate) async fn cmd_in(source: &str) -> anyhow::Result<()> {
    let cfg = crate::store::config::load_config()?;
    if cfg.session.capture != "ambient" {
        // Legacy manual mode: identical to the old behavior.
        let session = crate::session::start(source)?;
        eprintln!("● Recording session {}", &session.id[..8.min(session.id.len())]);
        eprintln!("  Run `mur out` to stop and extract, or `mur session stop` to discard.");
        return Ok(());
    }

    // Ambient mode: recording is always on — `mur in` marks importance.
    let session_dir = crate::paths::mur_root(None).join("session");
    std::fs::create_dir_all(&session_dir)?;

    // Mark the most recent recording if it saw activity in the last 10 minutes;
    // otherwise leave a marker the next captured event consumes.
    let recent = crate::session::list_recordings()?
        .into_iter()
        .find(|r| {
            r.modified
                .elapsed()
                .map(|e| e.as_secs() < 600)
                .unwrap_or(false)
        });
    match recent {
        Some(r) => {
            let meta = crate::session::update_marked(&r.id, true)?;
            eprintln!(
                "★ Session \"{}\" marked — the harvest gate will not skip it.",
                meta.title.as_deref().unwrap_or(&r.id[..8.min(r.id.len())])
            );
        }
        None => {
            std::fs::write(session_dir.join(crate::session::ambient::MARK_NEXT_FILE), "")?;
            eprintln!("★ Next session will be marked. (Recording is always on — see `mur session list`.)");
        }
    }
    Ok(())
}
```

Add the small setter to `mur-core/src/session/mod.rs` (next to `update_meta`, line 280):

```rust
/// Set or clear the `marked` flag on a session's meta.
pub fn update_marked(id: &str, marked: bool) -> Result<SessionMeta> {
    let mut meta =
        load_meta(id).ok_or_else(|| anyhow::anyhow!("No meta found for session '{}'", id))?;
    meta.marked = marked;
    save_meta(&meta)?;
    Ok(meta)
}
```

Note: `MARK_NEXT_FILE` is `pub(crate)` in ambient.rs — it already is, per Task 5.

- [ ] **Step 7: Full check**

Run: `cargo nextest run -p mur-core session && cargo clippy -p mur-core -- -D warnings`
Expected: PASS / clean.

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/cli/mod.rs mur-core/src/dispatch.rs mur-core/src/cmd/session.rs mur-core/src/session/mod.rs
git commit -m "feat(session): retention gc command; mur in becomes an importance marker in ambient mode"
```

---

## W2 — Harvest inbox

### Task 8: `harvest/skeleton.rs` — literal-stripping step skeletons

**Files:**
- Create: `mur-core/src/harvest/skeleton.rs`
- Create: `mur-core/src/harvest/mod.rs` (module shell, filled in Task 10)
- Modify: `mur-core/src/lib.rs` — add `pub mod harvest;` (alphabetical, after `pub mod federation;` line 33)

- [ ] **Step 1: Create module shell + skeleton with tests**

`mur-core/src/harvest/mod.rs` (shell for now):

```rust
//! Harvest pipeline (spec 2026-06-11 §3.2): heuristic gate over ambient
//! recordings → skeletonized workflow proposals → `mur out` review.

pub mod gate;
pub mod proposal;
pub mod skeleton;
```

`mur-core/src/harvest/skeleton.rs`:

```rust
//! Command skeletons: strip volatile literals so recurring procedures compare
//! equal across sessions (v2 spec Layer 2 normalization, heuristic subset).

use crate::session::SessionEvent;

/// Replace volatile literals in a shell command with placeholders.
pub fn skeletonize_command(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                // consume to closing quote (or end)
                for q in chars.by_ref() {
                    if q == c {
                        break;
                    }
                }
                out.push_str("<STR>");
            }
            _ => out.push(c),
        }
    }
    // token-level passes
    out.split_whitespace()
        .map(|tok| {
            if tok.starts_with('/') && tok.len() > 1 {
                "<PATH>"
            } else if tok.len() >= 12
                && tok.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            {
                "<ID>"
            } else if !tok.is_empty() && tok.chars().all(|c| c.is_ascii_digit()) {
                "<N>"
            } else {
                tok
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract an ordered, consecutive-deduped list of skeletonized commands from
/// a session's tool_call events. Non-shell tools become `tool:<Name>` markers.
pub fn steps_from_events(events: &[SessionEvent]) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();
    for e in events {
        if e.event_type != "tool_call" {
            continue;
        }
        let step = match e.tool.as_deref() {
            Some("Bash") | Some("shell") => {
                let cmd = serde_json::from_str::<serde_json::Value>(&e.content)
                    .ok()
                    .and_then(|v| {
                        v.get("command")
                            .and_then(|c| c.as_str())
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| e.content.clone());
                skeletonize_command(&cmd)
            }
            Some(other) => format!("tool:{}", other),
            None => continue,
        };
        if steps.last().map(|s| s.as_str()) != Some(step.as_str()) {
            steps.push(step);
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionEvent;

    fn tool_event(tool: &str, content: &str) -> SessionEvent {
        SessionEvent {
            timestamp: 0,
            event_type: "tool_call".to_string(),
            tool: Some(tool.to_string()),
            content: content.to_string(),
            working_dir: None,
            git_branch: None,
            exit_code: None,
        }
    }

    #[test]
    fn strips_quotes_paths_ids_numbers() {
        assert_eq!(
            skeletonize_command(r#"fly deploy --app "my-api" --wait 300"#),
            "fly deploy --app <STR> --wait <N>"
        );
        assert_eq!(
            skeletonize_command("cat /Users/d/x.txt"),
            "cat <PATH>"
        );
        assert_eq!(
            skeletonize_command("git checkout 0123abcd4567ef89"),
            "git checkout <ID>"
        );
    }

    #[test]
    fn steps_dedupe_consecutive_and_mark_tools() {
        let events = vec![
            tool_event("Bash", r#"{"command":"cargo build"}"#),
            tool_event("Bash", r#"{"command":"cargo build"}"#),
            tool_event("Read", "src/main.rs"),
            tool_event("Bash", r#"{"command":"cargo test"}"#),
        ];
        assert_eq!(
            steps_from_events(&events),
            vec!["cargo build", "tool:Read", "cargo test"]
        );
    }
}
```

`mur-core/src/harvest/gate.rs` and `proposal.rs` must exist for `mod.rs` to compile — create them as empty files for now (filled in Tasks 9–10):

```rust
//! (filled in by Task 9/10)
```

Add to `mur-core/src/lib.rs` after line 33 (`pub mod federation;`):

```rust
pub mod harvest;
```

- [ ] **Step 2: Run tests** — `cargo nextest run -p mur-core harvest::skeleton` → PASS (2 tests).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/harvest/ mur-core/src/lib.rs
git commit -m "feat(harvest): command skeletonization for step extraction"
```

---

### Task 9: `harvest/gate.rs` + `harvest/proposal.rs`

**Files:**
- Modify: `mur-core/src/harvest/gate.rs`
- Modify: `mur-core/src/harvest/proposal.rs`

- [ ] **Step 1: Implement the gate with tests**

`mur-core/src/harvest/gate.rs`:

```rust
//! Heuristic harvest gate — zero-token port of `session_worth_analyzing`
//! (spec §3.2: gate defaults to heuristics; LLM gating is a later, optional
//! enhancement). Marked sessions pass unconditionally (§3.3).

use mur_common::config::HarvestCfg;

use crate::session::{SessionEvent, SessionMeta};

pub struct GateDecision {
    pub pass: bool,
    pub reason: String,
}

/// Substrings that identify mur's own bookkeeping noise in event content.
const NOISE_MARKERS: &[&str] = &[
    "mur session",
    "mur sync",
    "mur context",
    "mur inject",
    "/mur:in",
    "/mur:out",
    "/mur-in",
    "/mur-out",
    "[stop:",
    "turn_end",
];

pub fn gate(events: &[SessionEvent], meta: &SessionMeta, cfg: &HarvestCfg) -> GateDecision {
    if meta.marked {
        return GateDecision {
            pass: true,
            reason: "marked via mur in".to_string(),
        };
    }

    let non_noise = events
        .iter()
        .filter(|e| {
            let lower = e.content.to_lowercase();
            !NOISE_MARKERS.iter().any(|n| lower.contains(n))
        })
        .count();
    let tool_calls = events.iter().filter(|e| e.event_type == "tool_call").count();
    let duration_secs = match (events.first(), events.last()) {
        (Some(f), Some(l)) if l.timestamp >= f.timestamp => {
            ((l.timestamp - f.timestamp) / 1000) as i64
        }
        _ => 0,
    };

    // Conservative: skip only when ALL signals are below threshold
    // (same shape as session_worth_analyzing, thresholds from config).
    if non_noise < cfg.min_events
        && meta.user_turns < cfg.min_user_turns
        && duration_secs < cfg.min_duration_secs
        && tool_calls == 0
    {
        return GateDecision {
            pass: false,
            reason: format!(
                "{} events, {} turns, {}s, {} tool calls",
                non_noise, meta.user_turns, duration_secs, tool_calls
            ),
        };
    }
    GateDecision {
        pass: true,
        reason: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(user_turns: usize, marked: bool) -> SessionMeta {
        SessionMeta {
            id: "s".into(),
            source: "claude".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            stopped_at: None,
            title: None,
            tools_used: vec![],
            user_turns,
            assistant_turns: 0,
            marked,
            gated_at: None,
            harvested_at: None,
        }
    }

    fn event(ts: u64, event_type: &str, content: &str) -> SessionEvent {
        SessionEvent {
            timestamp: ts,
            event_type: event_type.into(),
            tool: None,
            content: content.into(),
            working_dir: None,
            git_branch: None,
            exit_code: None,
        }
    }

    #[test]
    fn tiny_session_fails_gate() {
        let cfg = HarvestCfg::default();
        let events = vec![event(0, "user", "hi")];
        assert!(!gate(&events, &meta(1, false), &cfg).pass);
    }

    #[test]
    fn tool_heavy_session_passes_gate() {
        let cfg = HarvestCfg::default();
        let events = vec![
            event(0, "user", "deploy"),
            event(1000, "tool_call", r#"{"command":"fly deploy"}"#),
        ];
        assert!(gate(&events, &meta(1, false), &cfg).pass);
    }

    #[test]
    fn marked_session_always_passes() {
        let cfg = HarvestCfg::default();
        assert!(gate(&[], &meta(0, true), &cfg).pass);
    }
}
```

- [ ] **Step 2: Implement the proposal store with tests**

`mur-core/src/harvest/proposal.rs`:

```rust
//! Workflow proposals — the harvest inbox at
//! `~/.mur/inbox/workflow-proposals/<session_id>.yaml`. Pure YAML files so the
//! Hub / companion nudge surface can read the same inbox (spec §3.2, §3.8).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    Pending,
    Accepted,
    Dismissed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Same as the source session id (one proposal per session).
    pub id: String,
    pub title: String,
    /// kebab-case workflow name suggestion.
    pub suggested_name: String,
    pub steps: Vec<String>,
    pub event_count: usize,
    pub duration_secs: i64,
    pub created_at: String,
    pub status: ProposalStatus,
    /// Existing workflow this proposal nearly duplicates (suggest merge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similar_to: Option<String>,
}

pub fn inbox_dir() -> PathBuf {
    crate::paths::mur_root(None)
        .join("inbox")
        .join("workflow-proposals")
}

pub fn save_in_dir(dir: &Path, p: &Proposal) -> Result<()> {
    fs::create_dir_all(dir)?;
    let yaml = serde_yaml::to_string(p)?;
    let tmp = dir.join(format!("{}.yaml.tmp", p.id));
    fs::write(&tmp, yaml)?;
    fs::rename(&tmp, dir.join(format!("{}.yaml", p.id))).context("persist proposal")?;
    Ok(())
}

pub fn list_in_dir(dir: &Path) -> Result<Vec<Proposal>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path)
            && let Ok(p) = serde_yaml::from_str::<Proposal>(&content)
        {
            out.push(p);
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

pub fn pending_in_dir(dir: &Path) -> Result<Vec<Proposal>> {
    Ok(list_in_dir(dir)?
        .into_iter()
        .filter(|p| p.status == ProposalStatus::Pending)
        .collect())
}

pub fn set_status_in_dir(dir: &Path, id: &str, status: ProposalStatus) -> Result<()> {
    let path = dir.join(format!("{}.yaml", id));
    let mut p: Proposal = serde_yaml::from_str(&fs::read_to_string(&path)?)?;
    p.status = status;
    save_in_dir(dir, &p)
}

/// Token-set Jaccard similarity over two step lists (zero-cost near-dup check).
pub fn step_similarity(a: &[String], b: &[String]) -> f32 {
    let ta: BTreeSet<&str> = a.iter().flat_map(|s| s.split_whitespace()).collect();
    let tb: BTreeSet<&str> = b.iter().flat_map(|s| s.split_whitespace()).collect();
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    inter / union
}

/// kebab-case a session title into a workflow name suggestion.
pub fn suggest_name(title: &str) -> String {
    let name: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let name = name.split('-').filter(|s| !s.is_empty()).take(6).collect::<Vec<_>>().join("-");
    if name.is_empty() { "captured-workflow".to_string() } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(id: &str, status: ProposalStatus) -> Proposal {
        Proposal {
            id: id.into(),
            title: "Deploy api".into(),
            suggested_name: "deploy-api".into(),
            steps: vec!["cargo build".into(), "fly deploy --app <STR>".into()],
            event_count: 12,
            duration_secs: 300,
            created_at: "2026-06-11T00:00:00Z".into(),
            status,
            similar_to: None,
        }
    }

    #[test]
    fn save_list_pending_set_status_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        save_in_dir(tmp.path(), &proposal("s1", ProposalStatus::Pending)).unwrap();
        save_in_dir(tmp.path(), &proposal("s2", ProposalStatus::Dismissed)).unwrap();

        assert_eq!(list_in_dir(tmp.path()).unwrap().len(), 2);
        assert_eq!(pending_in_dir(tmp.path()).unwrap().len(), 1);

        set_status_in_dir(tmp.path(), "s1", ProposalStatus::Accepted).unwrap();
        assert!(pending_in_dir(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn similarity_high_for_same_skeleton() {
        let a = vec!["cargo build".to_string(), "fly deploy --app <STR>".to_string()];
        let b = vec!["cargo build".to_string(), "fly deploy --app <STR>".to_string()];
        assert!(step_similarity(&a, &b) > 0.99);
        let c = vec!["npm test".to_string()];
        assert!(step_similarity(&a, &c) < 0.2);
    }

    #[test]
    fn suggest_name_kebabs_title() {
        assert_eq!(suggest_name("Fix hub dark-mode contrast!"), "fix-hub-dark-mode-contrast");
        assert_eq!(suggest_name("???"), "captured-workflow");
    }
}
```

- [ ] **Step 3: Run tests** — `cargo nextest run -p mur-core harvest::` → PASS (8 tests across the three modules).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/harvest/
git commit -m "feat(harvest): heuristic gate and proposal inbox store"
```

---

### Task 10: `harvest::scan` — gate unharvested sessions into proposals

**Files:**
- Modify: `mur-core/src/harvest/mod.rs`

- [ ] **Step 1: Write scan with tests**

Replace `mur-core/src/harvest/mod.rs` with:

```rust
//! Harvest pipeline (spec 2026-06-11 §3.2): heuristic gate over ambient
//! recordings → skeletonized workflow proposals → `mur out` review.
//!
//! `scan_in_dirs` is the testable core; `scan()` binds it to `~/.mur`. The
//! proposals it writes are also the candidate feed for the companion nudge
//! surface (2026-05-29 nudge spec) — that emission path is owned by the nudge
//! plan, not this module.

pub mod gate;
pub mod proposal;
pub mod skeleton;

use anyhow::Result;
use mur_common::config::HarvestCfg;
use std::path::Path;

pub struct ScanReport {
    pub scanned: usize,
    pub proposed: usize,
}

/// Gate every un-gated, idle session; write a proposal per passing session.
/// Sets `gated_at` on every scanned session so a session is judged exactly once.
pub fn scan_in_dirs(
    recordings_dir: &Path,
    inbox_dir: &Path,
    existing_workflow_steps: &[(String, Vec<String>)],
    cfg: &HarvestCfg,
) -> Result<ScanReport> {
    let mut report = ScanReport { scanned: 0, proposed: 0 };
    if !recordings_dir.exists() {
        return Ok(report);
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let idle_ms = (cfg.idle_minutes.max(0) as u64) * 60 * 1000;

    for entry in std::fs::read_dir(recordings_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let meta_path = recordings_dir.join(format!("{}.meta.json", id));
        let Some(mut meta) = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|c| serde_json::from_str::<crate::session::SessionMeta>(&c).ok())
        else {
            continue;
        };
        if meta.gated_at.is_some() {
            continue;
        }

        let events = match crate::session::read_events(&id) {
            Ok(e) => e,
            Err(_) => {
                // read_events resolves against the default dir; in tests we read directly.
                let content = std::fs::read_to_string(&path).unwrap_or_default();
                content
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect()
            }
        };

        // Idle check: session must be over (no event for idle_minutes), unless marked.
        let last_ts = events.last().map(|e| e.timestamp).unwrap_or(0);
        if !meta.marked && now_ms.saturating_sub(last_ts) < idle_ms {
            continue;
        }

        report.scanned += 1;
        let decision = gate::gate(&events, &meta, cfg);
        meta.gated_at = Some(chrono::Utc::now().to_rfc3339());
        let _ = std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?);

        if !decision.pass {
            continue;
        }
        let steps = skeleton::steps_from_events(&events);
        if steps.is_empty() {
            continue; // nothing procedural to propose
        }

        let similar_to = existing_workflow_steps
            .iter()
            .map(|(name, wsteps)| (name, proposal::step_similarity(&steps, wsteps)))
            .filter(|(_, sim)| *sim >= cfg.similarity_merge_threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(name, _)| name.clone());

        let title = meta.title.clone().unwrap_or_else(|| id.clone());
        let duration_secs = match (events.first(), events.last()) {
            (Some(f), Some(l)) if l.timestamp >= f.timestamp => {
                ((l.timestamp - f.timestamp) / 1000) as i64
            }
            _ => 0,
        };
        let p = proposal::Proposal {
            id: id.clone(),
            suggested_name: proposal::suggest_name(&title),
            title,
            steps,
            event_count: events.len(),
            duration_secs,
            created_at: chrono::Utc::now().to_rfc3339(),
            status: proposal::ProposalStatus::Pending,
            similar_to,
        };
        proposal::save_in_dir(inbox_dir, &p)?;
        report.proposed += 1;
    }
    Ok(report)
}

/// Bind scan to the real `~/.mur` layout and the workflow store.
pub fn scan() -> Result<ScanReport> {
    let cfg = crate::store::config::load_config()?;
    if !cfg.harvest.auto_gate {
        return Ok(ScanReport { scanned: 0, proposed: 0 });
    }
    let recordings = crate::paths::mur_root(None)
        .join("session")
        .join("recordings");
    let store = crate::store::workflow_yaml::WorkflowYamlStore::default_store()?;
    let existing: Vec<(String, Vec<String>)> = store
        .list_all()
        .unwrap_or_default()
        .into_iter()
        .map(|w| {
            let steps = w
                .steps
                .iter()
                .map(|s| s.command.clone().unwrap_or_else(|| s.description.clone()))
                .collect();
            (w.name.clone(), steps)
        })
        .collect();
    scan_in_dirs(&recordings, &proposal::inbox_dir(), &existing, &cfg.harvest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionEvent, SessionMeta};

    fn write_session(dir: &std::path::Path, id: &str, n_tools: usize, marked: bool) {
        std::fs::create_dir_all(dir).unwrap();
        let mut lines = vec![serde_json::to_string(&SessionEvent {
            timestamp: 1000,
            event_type: "user".into(),
            tool: None,
            content: "deploy the api".into(),
            working_dir: None,
            git_branch: None,
            exit_code: None,
        })
        .unwrap()];
        for i in 0..n_tools {
            lines.push(
                serde_json::to_string(&SessionEvent {
                    timestamp: 2000 + i as u64,
                    event_type: "tool_call".into(),
                    tool: Some("Bash".into()),
                    content: format!(r#"{{"command":"step-{} \"x\""}}"#, i),
                    working_dir: None,
                    git_branch: None,
                    exit_code: Some(0),
                })
                .unwrap(),
            );
        }
        std::fs::write(dir.join(format!("{}.jsonl", id)), lines.join("\n") + "\n").unwrap();
        let meta = SessionMeta {
            id: id.into(),
            source: "claude".into(),
            started_at: "2026-06-01T00:00:00Z".into(),
            stopped_at: None,
            title: Some("deploy the api".into()),
            tools_used: vec!["Bash".into()],
            user_turns: 3,
            assistant_turns: 3,
            marked,
            gated_at: None,
            harvested_at: None,
        };
        std::fs::write(
            dir.join(format!("{}.meta.json", id)),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn scan_proposes_once_and_marks_gated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rec = tmp.path().join("recordings");
        let inbox = tmp.path().join("inbox");
        write_session(&rec, "s1", 4, false);

        let cfg = mur_common::config::HarvestCfg {
            idle_minutes: 0, // session timestamps are ancient → idle
            ..Default::default()
        };
        let r1 = scan_in_dirs(&rec, &inbox, &[], &cfg).unwrap();
        assert_eq!(r1.scanned, 1);
        assert_eq!(r1.proposed, 1);

        // Second scan: gated_at set → nothing scanned, nothing duplicated.
        let r2 = scan_in_dirs(&rec, &inbox, &[], &cfg).unwrap();
        assert_eq!(r2.scanned, 0);
        assert_eq!(proposal::pending_in_dir(&inbox).unwrap().len(), 1);
    }

    #[test]
    fn near_duplicate_gets_similar_to() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rec = tmp.path().join("recordings");
        let inbox = tmp.path().join("inbox");
        write_session(&rec, "s2", 3, false);

        let cfg = mur_common::config::HarvestCfg { idle_minutes: 0, ..Default::default() };
        let existing = vec![(
            "deploy-api".to_string(),
            vec![
                "step-0 <STR>".to_string(),
                "step-1 <STR>".to_string(),
                "step-2 <STR>".to_string(),
            ],
        )];
        scan_in_dirs(&rec, &inbox, &existing, &cfg).unwrap();
        let pending = proposal::pending_in_dir(&inbox).unwrap();
        assert_eq!(pending[0].similar_to.as_deref(), Some("deploy-api"));
    }
}
```

- [ ] **Step 2: Run tests** — `cargo nextest run -p mur-core harvest::` → PASS.

- [ ] **Step 3: Wire scan into `mur session gc`**

In `mur-core/src/cmd/session.rs::cmd_session_gc` (Task 7), append before `Ok(())`:

```rust
    if let Ok(report) = crate::harvest::scan()
        && report.proposed > 0
    {
        eprintln!("harvest: {} new workflow proposal(s)", report.proposed);
    }
```

- [ ] **Step 4: Run** — `cargo clippy -p mur-core -- -D warnings` → clean.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/harvest/mod.rs mur-core/src/cmd/session.rs
git commit -m "feat(harvest): scan unharvested sessions into workflow proposals"
```

---

### Task 11: `create_draft_workflow_with_steps` + new `mur out` review flow

**Files:**
- Modify: `mur-core/src/cmd/workflow.rs` (next to `create_draft_workflow_in`, line 988)
- Modify: `mur-core/src/cmd/session.rs` — `cmd_out` (line 318)

- [ ] **Step 1: Write the failing test for step-bearing drafts**

Append to `mod tests` in `mur-core/src/cmd/workflow.rs`:

```rust
#[test]
fn create_draft_workflow_with_steps_persists_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let store =
        crate::store::workflow_yaml::WorkflowYamlStore::new(tmp.path().join("workflows"))
            .unwrap();
    create_draft_workflow_with_steps_in(
        &store,
        "deploy-api",
        "Captured from session s1",
        "when deploying the api",
        &["s1".into()],
        &["cargo build".into(), "fly deploy --app <STR>".into()],
    )
    .unwrap();
    let wf = store.get("deploy-api").unwrap();
    assert_eq!(wf.steps.len(), 2);
    assert_eq!(wf.steps[0].order, 1);
    assert_eq!(wf.steps[0].command.as_deref(), Some("cargo build"));
    assert_eq!(wf.steps[1].command, None); // contains a placeholder → description only
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core create_draft_workflow_with_steps`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

Add to `mur-core/src/cmd/workflow.rs` below `create_draft_workflow` (line 1031):

```rust
/// Draft workflow with skeleton steps (harvest accept path). Steps containing
/// `<…>` placeholders are descriptions (the user fills variables on edit);
/// fully-literal steps double as runnable commands.
pub fn create_draft_workflow_with_steps_in(
    store: &crate::store::workflow_yaml::WorkflowYamlStore,
    name: &str,
    description: &str,
    trigger: &str,
    source_sessions: &[String],
    steps: &[String],
) -> anyhow::Result<()> {
    if store.exists(name) {
        return Ok(());
    }
    let base = KnowledgeBase {
        name: name.to_string(),
        description: description.to_string(),
        content: Content::Plain(trigger.to_string()),
        maturity: mur_common::knowledge::Maturity::Draft,
        ..Default::default()
    };
    let wf_steps = steps
        .iter()
        .enumerate()
        .map(|(i, s)| mur_common::workflow::Step {
            order: (i + 1) as u32,
            description: s.clone(),
            command: (!s.contains('<') && !s.starts_with("tool:")).then(|| s.clone()),
            ..Default::default()
        })
        .collect();
    let wf = mur_common::workflow::Workflow {
        base,
        steps: wf_steps,
        variables: vec![],
        source_sessions: source_sessions.to_vec(),
        trigger: trigger.to_string(),
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    };
    store.save(&wf)
}
```

- [ ] **Step 4: Run test** — `cargo nextest run -p mur-core create_draft_workflow_with_steps` → PASS.

- [ ] **Step 5: Rewrite `cmd_out`**

Replace the body of `cmd_out` in `mur-core/src/cmd/session.rs` (line 318). The `--action` escape hatch and legacy manual-mode stop are preserved; the default becomes the review inbox:

```rust
pub(crate) async fn cmd_out(action: Option<&str>, force: bool) -> anyhow::Result<()> {
    // Back-compat: explicit action keeps the old behavior verbatim.
    if let Some(action) = action {
        return cmd_out_execute(action, force).await;
    }

    // Legacy manual mode: stop the active session first (old `mur out` contract).
    if let Ok(Some(_)) = crate::session::get_active() {
        if let Ok(Some(id)) = crate::session::stop() {
            eprintln!("■ Stopped session {}", &id[..8.min(id.len())]);
        }
    }

    // Harvest: scan now (synchronous — the user asked), then review.
    let _ = crate::harvest::scan();
    let inbox = crate::harvest::proposal::inbox_dir();
    let pending = crate::harvest::proposal::pending_in_dir(&inbox)?;

    if pending.is_empty() {
        eprintln!("✓ Nothing to harvest — no pending workflow proposals.");
        eprintln!("  (Recording is always on; see `mur session list`.)");
        return Ok(());
    }

    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    if !is_tty {
        eprintln!("◆ {} pending workflow proposal(s):", pending.len());
        for p in &pending {
            eprintln!(
                "  {}  \"{}\" — {} steps{}",
                &p.id[..8.min(p.id.len())],
                p.title,
                p.steps.len(),
                p.similar_to
                    .as_deref()
                    .map(|s| format!(" (≈ existing `{}`)", s))
                    .unwrap_or_default()
            );
        }
        eprintln!("Run `mur out` in a terminal to review, or `mur out --action analyze` for LLM analysis.");
        return Ok(());
    }

    for p in pending {
        eprintln!();
        eprintln!("◆ \"{}\"  ({} events · {}m)", p.title, p.event_count, p.duration_secs / 60);
        for (i, s) in p.steps.iter().enumerate().take(8) {
            eprintln!("    {}. {}", i + 1, s);
        }
        if p.steps.len() > 8 {
            eprintln!("    … {} more", p.steps.len() - 8);
        }
        if let Some(similar) = &p.similar_to {
            eprintln!("  ⚠ near-duplicate of existing `{}` — consider merging instead", similar);
        }

        let items = &["✓ Accept as draft workflow", "⏭ Skip", "✗ Quit review"];
        let choice = dialoguer::Select::new()
            .with_prompt(format!("Save as `{}`?", p.suggested_name))
            .items(items)
            .default(0)
            .interact()
            .unwrap_or(2);
        match choice {
            0 => {
                crate::cmd::workflow::create_draft_workflow_with_steps(
                    &p.suggested_name,
                    &format!("Captured from session {}", &p.id[..8.min(p.id.len())]),
                    &p.title,
                    &[p.id.clone()],
                    &p.steps,
                )?;
                crate::harvest::proposal::set_status_in_dir(
                    &inbox,
                    &p.id,
                    crate::harvest::proposal::ProposalStatus::Accepted,
                )?;
                mark_harvested(&p.id);
                eprintln!("  ✓ Draft saved — edit: ~/.mur/workflows/{}.yaml · run: mur run {}",
                    p.suggested_name, p.suggested_name);
            }
            1 => {
                crate::harvest::proposal::set_status_in_dir(
                    &inbox,
                    &p.id,
                    crate::harvest::proposal::ProposalStatus::Dismissed,
                )?;
                mark_harvested(&p.id);
            }
            _ => break,
        }
    }
    Ok(())
}

/// Stamp `harvested_at` so retention GC may reclaim the recording.
fn mark_harvested(id: &str) {
    if let Some(mut meta) = crate::session::load_meta_pub(id) {
        meta.harvested_at = Some(chrono::Utc::now().to_rfc3339());
        let recordings = crate::paths::mur_root(None).join("session").join("recordings");
        if let Ok(json) = serde_json::to_string_pretty(&meta) {
            let _ = std::fs::write(recordings.join(format!("{}.meta.json", id)), json);
        }
    }
}
```

Also add the default-store convenience next to `create_draft_workflow` in `cmd/workflow.rs`:

```rust
/// Convenience over the default store.
pub fn create_draft_workflow_with_steps(
    name: &str,
    description: &str,
    trigger: &str,
    source_sessions: &[String],
    steps: &[String],
) -> anyhow::Result<()> {
    let store = crate::store::workflow_yaml::WorkflowYamlStore::default_store()?;
    create_draft_workflow_with_steps_in(&store, name, description, trigger, source_sessions, steps)
}
```

> The old interactive Analyze/Export/Skip menu body inside `cmd_out` (the `dialoguer::Select` over `items` at old lines 405-470) is now dead — delete it. `cmd_out_execute` stays (it serves `--action`).

- [ ] **Step 6: Run** — `cargo nextest run -p mur-core && cargo clippy -p mur-core -- -D warnings` → PASS / clean.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/workflow.rs mur-core/src/cmd/session.rs
git commit -m "feat(harvest): mur out becomes the proposal review inbox; drafts carry skeleton steps"
```

---

### Task 12: §3.8 tier-1 SessionStart hint

**Files:**
- Modify: `mur-core/src/cmd/hook.rs` — `cmd_hook_session_start` (line 262)

- [ ] **Step 1: Implement the hint**

In `cmd_hook_session_start`, after the existing `if !output.is_empty() { print!("{output}"); }` block, add:

```rust
    // §3.8 tier-1: one-line harvest hint (config-gated, zero tokens — counts files only).
    let hint_enabled = crate::store::config::load_config()
        .map(|c| c.harvest.session_start_hint)
        .unwrap_or(true);
    if hint_enabled
        && let Ok(pending) =
            crate::harvest::proposal::pending_in_dir(&crate::harvest::proposal::inbox_dir())
        && !pending.is_empty()
    {
        println!(
            "📥 {} workflow proposal(s) pending — run `mur out` to review.",
            pending.len()
        );
    }
```

- [ ] **Step 2: Manual smoke test**

```bash
MUR_HOME=$(mktemp -d) sh -c '
  mkdir -p "$MUR_HOME/inbox/workflow-proposals"
  printf "id: s1\ntitle: Deploy api\nsuggested_name: deploy-api\nsteps: [\"cargo build\"]\nevent_count: 5\nduration_secs: 60\ncreated_at: \"2026-06-11T00:00:00Z\"\nstatus: pending\n" \
    > "$MUR_HOME/inbox/workflow-proposals/s1.yaml"
  echo "{}" | cargo run -q -p mur-core --bin mur -- hook session-start --tool claude | tail -1
'
```
Expected last line: `📥 1 workflow proposal(s) pending — run `mur out` to review.`

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/hook.rs
git commit -m "feat(hook): session-start pending-proposals hint (spec 3.8 tier 1)"
```

---

### Task 13: Skills + docs + final verification

**Files:**
- Modify: repo skill manifests for mur-in / mur-out (locate: `grep -rln "mur session in" --include=SKILL.md .` from repo root; skip this sub-step if none are in-repo)
- Modify: `docs/architecture/runtime-overview.md`
- Modify: `README.md`

- [ ] **Step 1: Update skill texts (if present in repo)**

For the mur-in skill body, replace the recording instructions with:

```markdown
# mur-in — Mark this session as important

Recording is always on (ambient capture via hooks). Run `mur in` to MARK the
current session so the harvest gate never skips it. You do NOT need to record
events manually — do not call `mur session record`.
```

For the mur-out skill body:

```markdown
# mur-out — Review what MUR learned

Run `mur out` to review pending workflow proposals harvested from recent
sessions (accept → draft workflow, runnable via `mur run <name>`).
`mur out --action analyze` still runs LLM pattern extraction on the most
recent session.
```

- [ ] **Step 2: Document the flow**

In `docs/architecture/runtime-overview.md`, find the session/recording section (`grep -n "session" docs/architecture/runtime-overview.md | head`) and add:

```markdown
### Ambient capture & harvest (2026-06-11)

Hooks write every prompt / post-tool / stop event to
`~/.mur/session/recordings/<session_id>.jsonl` (config `session.capture:
ambient|manual|off`; secrets scrubbed at write; `session.retention_days` GC via
`mur session gc`, spawned from the session-start hook). `mur in` marks the
current session as important. `mur session gc` also runs the zero-token
heuristic harvest gate (`harvest.*` config) and writes workflow proposals to
`~/.mur/inbox/workflow-proposals/`; `mur out` reviews them — accept creates a
draft workflow with skeleton steps. Spec:
`docs/superpowers/specs/2026-06-11-mur-ambient-capture-and-harvest-design.md`.
```

In `README.md`, update any description of `mur in` / `mur out` (locate: `grep -n "mur in\|mur out" README.md`) to one line each: `mur in` = mark the current session as important; `mur out` = review harvested workflow proposals.

- [ ] **Step 3: Full verification**

```bash
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo nextest run -p mur-core
cargo nextest run -p mur-common
```
Expected: all clean / PASS.

- [ ] **Step 4: End-to-end smoke (the payoff moment)**

```bash
MUR_HOME=$(mktemp -d) sh -c '
  B="cargo run -q -p mur-core --bin mur --"
  echo "{\"session_id\":\"e2e-1\",\"prompt\":\"deploy the api\",\"cwd\":\"$PWD\"}" | $B hook prompt --tool claude
  echo "{\"session_id\":\"e2e-1\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"cargo build\"},\"tool_response\":{\"exit_code\":0}}" | $B hook tool --tool claude
  echo "{\"session_id\":\"e2e-1\",\"tool_name\":\"Bash\",\"tool_input\":{\"command\":\"fly deploy --app my-api\"},\"tool_response\":{\"exit_code\":0}}" | $B hook tool --tool claude
  $B session gc
  $B out </dev/null   # non-TTY: prints the pending list
'
```
Expected: `mur out` prints `◆ 1 pending workflow proposal(s)` with the e2e-1 session, steps `cargo build` and `fly deploy --app <STR>`.
Note: the gate needs the session idle for `harvest.idle_minutes` (30) — for the smoke test write `harvest:\n  idle_minutes: 0\n` into `$MUR_HOME/config.yaml` before `session gc`.

- [ ] **Step 5: Commit**

```bash
git add docs/architecture/runtime-overview.md README.md skills/
git commit -m "docs: ambient capture & harvest flow (W1+W2)"
```

---

## Out of scope (tracked, not built here)

- **LLM Extract + §3.7 caps enforcement** — lands with workflow-engine v2 P5a; the config fields ship now so the YAML contract is stable.
- **Companion nudge emission** — the proposals inbox is the candidate feed; emission belongs to the nudge spec's own plan.
- **W3 (v2 P1a–P4), W4 server runs, W5 dispatch/HITL** — separate plans per spec §4.
- **`mur-out` Hub/dashboard inbox page** — Hub work, after Phase 3 merges.

## Self-review notes

- Spec §3.1: ambient keying ✓ (T5), Layer-1 fields ✓ (T2/T3/T5), transcript tail ✓ (T5), scrub-at-write ✓ (T4/T5), retention ✓ (T7), config ✓ (T1), per-project `capture` override is honored via `load_config()` (config.yaml already supports project overrides through existing config machinery — no extra work).
- Spec §3.2: gate→proposal ✓ (T9/T10), `mur out` review default ✓ (T11), near-dup merge suggestion ✓ (T10), zero-token ✓ (no LLM calls anywhere).
- Spec §3.3: `mur in` marker + gate bypass ✓ (T7/T9); `mur out --action …` back-compat ✓ (T11).
- Spec §3.8 tier 1 ✓ (T12).
- Type consistency: `SessionEvent{working_dir,git_branch,exit_code}` (T2) used by T4/T5/T8/T9/T10 consistently; `HarvestCfg` field names in T1 match usages in T9/T10/T11/T12; `create_draft_workflow_with_steps[_in]` defined T11 before use in T11's `cmd_out`.
