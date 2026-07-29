# murmur `--fleet` Status Rail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `murmur --fleet <name>` chats with an agent while a status band folds the fleet's shared channel into per-member state — one line of job progress at rest, expanding only when a member is blocked.

**Architecture:** A new `FleetRail` type polls the fleet's single channel (`fleet-<name>`) on the same 700 ms cadence as the existing `Follow`, gated on the channel log's size and the job directory's mtime so an idle tick costs two `metadata()` calls. Two pure functions do the work — `fold_members()` turns channel events into member rows, `jobs_line()` turns the job store into the collapsed summary — and a layout band renders the result between the transcript and the composer.

**Tech Stack:** Rust 2024, ratatui 0.29 (Inline viewport), `mur-common::channel` event model, `mur-channel` store, `serde_yaml`, `cargo nextest`.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-29-murmur-fleet-rail-design.md`. Read it before Task 1.
- Rust edition 2024 — `let` chains are stable (`if let … && let …`).
- No hardcoded values: poll cadence reuses `follow::POLL_INTERVAL`; the expanded-row cap is the named constant `MAX_EXPANDED_ROWS`.
- Single source file ≤ 800 lines.
- Signature verification is never bypassed: every event the rail folds passes `crate::channel_verify::verify_event`, with `require_sig` read exactly as `hitl/gate.rs:84` reads it.
- The rail never propagates an error into the event loop. Any failure degrades to a one-line notice.
- Build env for every `cargo` command in this plan:
  ```bash
  export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
  export ORT_STRATEGY=download
  export MUR_WEB_DIST=$HOME/Projects/mur-web/dist
  export RUST_MIN_STACK=33554432
  ```
- Branch off `main`; one commit per task; re-check `git branch --show-current` before every commit (main advances mid-session).

---

## File Structure

| File | Responsibility |
|---|---|
| `mur-core/src/cmd/agent/cli/fleet_rail.rs` **(new, ~260 lines)** | Everything rail: `MemberState`, `MemberRow`, `RailView`, the pure `fold_members()` / `jobs_line()`, and the polling `FleetRail`. |
| `mur-core/src/cli/agent.rs:140` | Add `fleet: Option<String>` to the `Cli` action. |
| `mur-core/src/dispatch.rs:1547` | Pass it through. |
| `mur-core/src/cmd/agent/cli/mod.rs:111` | `cmd_cli` takes `fleet`, validates it, constructs the rail, polls it in the event loop. |
| `mur-core/src/cmd/agent/cli/app.rs` | `App.fleet: Option<FleetRail>`. |
| `mur-core/src/cmd/agent/cli/ui.rs` | `fleet_rail_height()`, `render_fleet_rail()`, layout band, and the `band_inner_rows()` subtraction. |

Rail types live in one file because they change together: a new member state needs a new render arm and a new height case in the same edit.

---

### Task 1: `--fleet` reaches the TUI and rejects an unknown fleet

Plumbing only — nothing renders yet. Ends with a flag that parses, validates, and is ignored.

**Files:**
- Modify: `mur-core/src/cli/agent.rs:140-166` (the `Cli` variant)
- Modify: `mur-core/src/dispatch.rs:1547`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs:111-118` (`cmd_cli` signature)
- Test: `mur-core/src/cli/agent.rs` (existing `mod tests` at the bottom)

**Interfaces:**
- Produces: `AgentAction::Cli { …, fleet: Option<String> }` and `cmd_cli(names, resume, auto, skin, plain, budget_usd, auto_reads, fleet)`.

- [ ] **Step 1: Write the failing test**

In `mur-core/src/cli/agent.rs`, inside the existing `mod tests`:

```rust
#[test]
fn cli_action_parses_fleet_flag() {
    let AgentAction::Cli { names, fleet, .. } =
        parse_cli_action(&["mur", "agent", "cli", "mur", "--fleet", "develop"])
    else {
        panic!("expected Cli action");
    };
    assert_eq!(names, vec!["mur".to_string()]);
    assert_eq!(fleet.as_deref(), Some("develop"));

    // Absent by default — a plain murmur must not become fleet-aware.
    let AgentAction::Cli { fleet, .. } = parse_cli_action(&["mur", "agent", "cli", "mur"]) else {
        panic!("expected Cli action");
    };
    assert_eq!(fleet, None);
}
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo nextest run -p mur-core --lib cli_action_parses_fleet_flag
```
Expected: FAIL — `struct AgentAction::Cli has no field named fleet`.

- [ ] **Step 3: Add the field**

In `mur-core/src/cli/agent.rs`, inside the `Cli { … }` variant, after `auto_reads`:

```rust
        /// Watch a fleet's shared channel in a status band: job progress at
        /// rest, expanding when a member is blocked. Names the fleet, not the
        /// channel — `--fleet develop` watches `fleet-develop`.
        #[arg(long)]
        fleet: Option<String>,
```

- [ ] **Step 4: Thread it through the dispatcher**

`mur-core/src/dispatch.rs:1547` — add `fleet` to the destructured fields and the call:

```rust
        } => {
            cmd::agent::cmd_cli(
                &names, resume, auto, skin, plain, budget_usd, auto_reads, fleet,
            )
            .await?
        }
```

- [ ] **Step 5: Accept and validate it in `cmd_cli`**

`mur-core/src/cmd/agent/cli/mod.rs:111` — add the parameter, then validate right after `home` is resolved (the existing `let home = super::resolve_mur_home()?;`):

```rust
pub async fn cmd_cli(
    names: &[String],
    resume: bool,
    auto: bool,
    skin: Option<String>,
    plain: bool,
    budget_usd: Option<f64>,
    auto_reads: bool,
    fleet: Option<String>,
) -> Result<()> {
```

and after `let home = …`:

```rust
    // Fail loudly on an unknown fleet. Degrading to a plain murmur would leave
    // the user believing they are watching a fleet when they are not.
    if let Some(f) = fleet.as_deref() {
        crate::cmd::fleet::store::load_fleet(&home, f)
            .with_context(|| format!("--fleet {f}"))?;
    }
```

If the multi-agent branch above (`names.len() > 1`) is taken, print the same shape of note the other single-agent-only flags print:

```rust
        if fleet.is_some() {
            eprintln!(
                "note: --fleet is only shown in the single-agent TUI; it is ignored when opening multiple agents."
            );
        }
```

- [ ] **Step 6: Run the test and the whole CLI-parse suite**

```bash
cargo nextest run -p mur-core --lib cli::agent
```
Expected: PASS, no other test regressed.

- [ ] **Step 7: Commit**

```bash
git branch --show-current   # confirm the feature branch
git add mur-core/src/cli/agent.rs mur-core/src/dispatch.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(murmur): accept --fleet <name> and validate it exists"
```

---

### Task 2: fold channel events into member rows

The heart of the feature, and pure: `&[ChannelEvent]` in, `Vec<MemberRow>` out. No I/O, no clock.

**Files:**
- Create: `mur-core/src/cmd/agent/cli/fleet_rail.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (add `mod fleet_rail;` next to `mod follow;`)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `pub enum MemberState { Blocked { summary: String, hitl_id: String }, Working { tool: Option<String>, since: DateTime<Utc> }, Done, Failed }`
  - `pub struct MemberRow { pub agent: String, pub state: MemberState }`
  - `pub fn fold_members(events: &[ChannelEvent]) -> Vec<MemberRow>`
  - `pub const MAX_EXPANDED_ROWS: usize = 6;`

- [ ] **Step 1: Write the failing tests**

Create `mur-core/src/cmd/agent/cli/fleet_rail.rs` containing only the test module plus the imports it needs:

```rust
//! `--fleet` status rail: folds a fleet's shared channel into per-member state.

use chrono::{DateTime, Utc};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: u64, actor: ChannelActor, kind: EventKind, payload: serde_json::Value) -> ChannelEvent {
        ChannelEvent {
            seq,
            ts: DateTime::parse_from_rfc3339("2026-07-29T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            actor,
            kind,
            payload,
            idempotency_key: None,
            sig: None,
            key_version: None,
        }
    }

    fn agent(id: &str) -> ChannelActor {
        ChannelActor::Agent { id: id.into() }
    }

    #[test]
    fn state_changes_map_to_member_states() {
        let evs = vec![
            ev(1, agent("qa"), EventKind::StateChange, json!({"from": "submitted", "to": "working"})),
            ev(2, agent("backend"), EventKind::StateChange, json!({"from": "working", "to": "completed"})),
            ev(3, agent("dataml"), EventKind::StateChange, json!({"from": "working", "to": "failed"})),
            ev(4, agent("pm"), EventKind::StateChange, json!({"from": "working", "to": "canceled"})),
        ];
        let rows = fold_members(&evs);
        let by = |n: &str| rows.iter().find(|r| r.agent == n).unwrap().state.clone();
        assert!(matches!(by("qa"), MemberState::Working { .. }));
        assert!(matches!(by("backend"), MemberState::Done));
        // canceled and rejected collapse into failed — the user only needs
        // "it did not finish".
        assert!(matches!(by("dataml"), MemberState::Failed));
        assert!(matches!(by("pm"), MemberState::Failed));
    }

    #[test]
    fn a_hitl_request_blocks_and_its_response_unblocks() {
        let req = json!({"hitl_id": "h1", "tool_name": "bash", "summary": "cargo publish", "action_hash": "x", "tier": "write"});
        let evs = vec![
            ev(1, agent("qa"), EventKind::StateChange, json!({"to": "working"})),
            ev(2, agent("qa"), EventKind::HitlRequest, req),
        ];
        let rows = fold_members(&evs);
        match &rows[0].state {
            MemberState::Blocked { summary, hitl_id } => {
                assert_eq!(hitl_id, "h1");
                assert!(summary.contains("cargo publish"));
            }
            other => panic!("expected blocked, got {other:?}"),
        }

        // The approval is written by the HUMAN, not by the blocked agent, so
        // clearing must key on hitl_id — never on the actor.
        let mut evs = evs;
        evs.push(ev(
            3,
            ChannelActor::Human { name: "david".into() },
            EventKind::HitlResponse,
            json!({"hitl_id": "h1", "allow": true, "surface": "cli"}),
        ));
        let rows = fold_members(&evs);
        assert!(matches!(rows[0].state, MemberState::Working { .. }));
    }

    #[test]
    fn tool_calls_annotate_the_working_row() {
        let evs = vec![
            ev(1, agent("qa"), EventKind::StateChange, json!({"to": "working"})),
            ev(2, agent("qa"), EventKind::ToolCall, json!({"tool": "bash", "command": "cargo test"})),
        ];
        let rows = fold_members(&evs);
        match &rows[0].state {
            MemberState::Working { tool, .. } => assert_eq!(tool.as_deref(), Some("cargo test")),
            other => panic!("expected working, got {other:?}"),
        }
    }

    #[test]
    fn human_and_system_actors_never_become_rows() {
        let evs = vec![
            ev(1, ChannelActor::Human { name: "david".into() }, EventKind::Message, json!({"text": "go"})),
            ev(2, ChannelActor::System, EventKind::StateChange, json!({"to": "working"})),
        ];
        assert!(fold_members(&evs).is_empty());
    }

    #[test]
    fn blocked_sorts_first_then_working_then_finished() {
        let evs = vec![
            ev(1, agent("aaa_done"), EventKind::StateChange, json!({"to": "completed"})),
            ev(2, agent("bbb_working"), EventKind::StateChange, json!({"to": "working"})),
            ev(3, agent("ccc_blocked"), EventKind::HitlRequest, json!({"hitl_id": "h1", "tool_name": "bash", "summary": "rm", "action_hash": "x", "tier": "write"})),
        ];
        let rows = fold_members(&evs);
        let names: Vec<&str> = rows.iter().map(|r| r.agent.as_str()).collect();
        assert_eq!(names, vec!["ccc_blocked", "bbb_working", "aaa_done"]);
    }

    #[test]
    fn an_empty_channel_has_no_rows() {
        assert!(fold_members(&[]).is_empty());
    }
}
```

- [ ] **Step 2: Register the module and watch it fail**

Add to `mur-core/src/cmd/agent/cli/mod.rs`, next to the other `mod` declarations:

```rust
mod fleet_rail;
```

```bash
cargo nextest run -p mur-core --lib fleet_rail
```
Expected: FAIL — `cannot find function fold_members`.

- [ ] **Step 3: Implement the fold**

Above the test module in `fleet_rail.rs`:

```rust
/// Most member rows shown when the rail expands. Blocked sorts first, so
/// whatever is truncated is the least urgent.
pub const MAX_EXPANDED_ROWS: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub enum MemberState {
    /// Waiting on a human. `hitl_id` is what `mur channel approve` takes.
    Blocked { summary: String, hitl_id: String },
    /// `tool` is the latest ToolCall's command; `since` is when the member
    /// last changed state, rendered as elapsed time so a dead runtime shows
    /// up as a growing number instead of a state we invented.
    Working {
        tool: Option<String>,
        since: DateTime<Utc>,
    },
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemberRow {
    pub agent: String,
    pub state: MemberState,
}

impl MemberState {
    /// Sort key: blocked first, then working, then finished.
    fn rank(&self) -> u8 {
        match self {
            MemberState::Blocked { .. } => 0,
            MemberState::Working { .. } => 1,
            MemberState::Done | MemberState::Failed => 2,
        }
    }
}

/// First non-empty string field among `keys`.
fn field<'a>(ev: &'a ChannelEvent, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| ev.payload.get(*k).and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
}

/// Fold a channel's events into one row per agent that has acted.
///
/// Only `ChannelActor::Agent` becomes a row: `Human` events are the user's own
/// turns and `System` events are the executor's bookkeeping. A member with no
/// events is absent rather than "idle" — silence is not a state we can read.
pub fn fold_members(events: &[ChannelEvent]) -> Vec<MemberRow> {
    // Insertion-ordered so the sort below is the only thing that reorders.
    let mut rows: Vec<MemberRow> = Vec::new();
    let mut index = |rows: &mut Vec<MemberRow>, id: &str, ts: DateTime<Utc>| -> usize {
        if let Some(i) = rows.iter().position(|r| r.agent == id) {
            return i;
        }
        rows.push(MemberRow {
            agent: id.to_string(),
            state: MemberState::Working {
                tool: None,
                since: ts,
            },
        });
        rows.len() - 1
    };

    for ev in events {
        // A HitlResponse is written by whoever approved — usually the human —
        // so it is matched by hitl_id across ALL rows, not by actor.
        if ev.kind == EventKind::HitlResponse
            && let Some(id) = field(ev, &["hitl_id"])
        {
            for row in rows.iter_mut() {
                if let MemberState::Blocked { hitl_id, .. } = &row.state
                    && hitl_id == id
                {
                    row.state = MemberState::Working {
                        tool: None,
                        since: ev.ts,
                    };
                }
            }
            continue;
        }

        let ChannelActor::Agent { id } = &ev.actor else {
            continue;
        };

        match ev.kind {
            EventKind::StateChange => {
                let Some(to) = field(ev, &["to"]) else { continue };
                let state = match to {
                    // ChannelState serializes kebab-case (see channel.rs tests).
                    "working" | "submitted" => MemberState::Working {
                        tool: None,
                        since: ev.ts,
                    },
                    "input-required" => MemberState::Blocked {
                        summary: "waiting for input".to_string(),
                        hitl_id: String::new(),
                    },
                    "completed" => MemberState::Done,
                    "failed" | "canceled" | "rejected" => MemberState::Failed,
                    _ => continue,
                };
                let i = index(&mut rows, id, ev.ts);
                rows[i].state = state;
            }
            EventKind::ToolCall => {
                let tool = field(ev, &["command", "tool", "description"]).map(str::to_string);
                let i = index(&mut rows, id, ev.ts);
                // A tool call only annotates a running member; it must not
                // resurrect one that already finished or unblock one waiting.
                if let MemberState::Working { since, .. } = rows[i].state {
                    rows[i].state = MemberState::Working { tool, since };
                }
            }
            EventKind::HitlRequest => {
                let i = index(&mut rows, id, ev.ts);
                rows[i].state = MemberState::Blocked {
                    summary: field(ev, &["summary", "tool_name"])
                        .unwrap_or("approval needed")
                        .to_string(),
                    hitl_id: field(ev, &["hitl_id"]).unwrap_or_default().to_string(),
                };
            }
            _ => {}
        }
    }

    rows.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then_with(|| a.agent.cmp(&b.agent))
    });
    rows
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo nextest run -p mur-core --lib fleet_rail
```
Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git branch --show-current
git add mur-core/src/cmd/agent/cli/fleet_rail.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(murmur): fold fleet channel events into per-member state"
```

---

### Task 3: the collapsed job-progress line

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/fleet_rail.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn jobs_line(fleet: &str, jobs: &[Job]) -> String`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` in `fleet_rail.rs`:

```rust
    use mur_common::fleet::{Job, JobStatus};

    fn job(id: &str, status: JobStatus) -> Job {
        Job {
            id: id.into(),
            text: "do the thing".into(),
            source: "cli".into(),
            status,
            created_at: "2026-07-29T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            run_id: None,
            result: None,
            error: None,
        }
    }

    #[test]
    fn jobs_line_counts_terminal_over_total() {
        let jobs = vec![
            job("1", JobStatus::Done),
            job("2", JobStatus::Failed),
            job("3", JobStatus::Running),
            job("4", JobStatus::Queued),
            job("5", JobStatus::Queued),
        ];
        // 2 of 5 have reached a terminal state; one of those failed.
        let line = jobs_line("develop", &jobs);
        assert!(line.contains("fleet · develop"), "got: {line}");
        assert!(line.contains("job 2/5"), "got: {line}");
        assert!(line.contains("1 ⏵ running"), "got: {line}");
        assert!(line.contains("1 ✖ failed"), "got: {line}");
    }

    #[test]
    fn jobs_line_says_not_run_yet_when_there_are_none() {
        let line = jobs_line("develop", &[]);
        assert!(line.contains("not run yet"), "got: {line}");
        assert!(line.contains("mur fleet run develop"), "got: {line}");
    }

    #[test]
    fn jobs_line_omits_the_failed_clause_when_nothing_failed() {
        let line = jobs_line("develop", &[job("1", JobStatus::Done)]);
        assert!(!line.contains("failed"), "got: {line}");
    }
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo nextest run -p mur-core --lib fleet_rail::tests::jobs_line
```
Expected: FAIL — `cannot find function jobs_line`.

- [ ] **Step 3: Implement it**

In `fleet_rail.rs` (add `use mur_common::fleet::{Job, JobStatus};` to the file's imports):

```rust
/// The always-present collapsed line: how far the fleet's work has got.
///
/// `2/5` is jobs in a terminal state over the total — the question a user asks
/// first ("how far along?"), answered by the slow-moving store rather than by
/// the event stream.
pub fn jobs_line(fleet: &str, jobs: &[Job]) -> String {
    if jobs.is_empty() {
        return format!("fleet · {fleet}   not run yet (mur fleet run {fleet})");
    }
    let total = jobs.len();
    let terminal = jobs.iter().filter(|j| j.status.is_terminal()).count();
    let running = jobs
        .iter()
        .filter(|j| j.status == JobStatus::Running)
        .count();
    let failed = jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Failed | JobStatus::Canceled))
        .count();
    let mut line = format!("fleet · {fleet}   job {terminal}/{total}");
    if running > 0 {
        line.push_str(&format!(" · {running} ⏵ running"));
    }
    if failed > 0 {
        line.push_str(&format!(" · {failed} ✖ failed"));
    }
    line
}
```

- [ ] **Step 4: Run the tests**

```bash
cargo nextest run -p mur-core --lib fleet_rail
```
Expected: 9 PASS.

- [ ] **Step 5: Commit**

```bash
git branch --show-current
git add mur-core/src/cmd/agent/cli/fleet_rail.rs
git commit -m "feat(murmur): job-progress summary line for the fleet rail"
```

---

### Task 4: `FleetRail` — the gated poller

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/fleet_rail.rs`

**Interfaces:**
- Consumes: `fold_members`, `jobs_line`, `MAX_EXPANDED_ROWS` (Tasks 2–3); `follow::POLL_INTERVAL`.
- Produces:
  - `pub struct RailView { pub jobs_line: String, pub members: Vec<MemberRow>, pub notice: Option<String> }`
  - `pub struct FleetRail` with `pub fn start(home: &Path, fleet: &str) -> Self`, `pub fn poll(&mut self, home: &Path, now: Instant) -> bool`, `pub fn view(&self) -> &RailView`, `pub fn next_poll(&self) -> Instant`

- [ ] **Step 1: Write the failing test**

Add to `mod tests`:

```rust
    use std::time::Instant;

    /// A fleet channel with one member. `create_for_fleet(fleet_name, router,
    /// members)` names the channel `fleet-<fleet_name>` itself — the rail must
    /// derive the same id from `--fleet dev`.
    fn seed_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        svc.create_for_fleet("dev", "mur", &["qa".to_string()])
            .unwrap();
        tmp
    }

    #[test]
    fn poll_reports_change_only_when_the_log_grows() {
        let tmp = seed_home();
        let now = Instant::now();
        let mut rail = FleetRail::start(tmp.path(), "dev");

        // First poll reads the (empty) channel and the (absent) job dir.
        assert!(rail.poll(tmp.path(), now), "first poll must produce a view");
        assert!(rail.view().members.is_empty());
        assert!(rail.view().jobs_line.contains("not run yet"));

        // Nothing changed → no work, no change reported.
        assert!(!rail.poll(tmp.path(), now));

        // A member acts → the next poll picks it up.
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        svc.append(
            "fleet-dev",
            ChannelActor::Agent { id: "qa".into() },
            EventKind::StateChange,
            serde_json::json!({"to": "working"}),
            None,
        )
        .unwrap();
        assert!(rail.poll(tmp.path(), now), "log grew → view must change");
        assert_eq!(rail.view().members.len(), 1);
        assert_eq!(rail.view().members[0].agent, "qa");
    }

    #[test]
    fn an_unreadable_channel_degrades_instead_of_failing() {
        let tmp = tempfile::TempDir::new().unwrap(); // no channel at all
        let mut rail = FleetRail::start(tmp.path(), "ghost");
        rail.poll(tmp.path(), Instant::now());
        assert!(rail.view().members.is_empty());
        // The rail says so on its own line; it never returns Err.
        assert!(rail.view().notice.is_some());
    }
```

- [ ] **Step 2: Run it and watch it fail**

```bash
cargo nextest run -p mur-core --lib fleet_rail::tests::poll
```
Expected: FAIL — `cannot find struct FleetRail`.

- [ ] **Step 3: Implement it**

In `fleet_rail.rs` (add the imports `use std::path::Path; use std::time::Instant; use super::follow::POLL_INTERVAL;`):

```rust
/// What the band renders. Recomputed only when the channel log or the job
/// store actually changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RailView {
    pub jobs_line: String,
    pub members: Vec<MemberRow>,
    /// Degraded-state text (unreadable channel, unreadable jobs). Rendered in
    /// place of detail; never an error the caller has to handle.
    pub notice: Option<String>,
}

/// Polls one fleet's channel and job store, folding both into a `RailView`.
///
/// Deliberately a separate type from `Follow`: that one turns events into
/// transcript lines (history, reaches scrollback), this one folds them into
/// current state (repainted every frame, never flushed). Keeping them apart also
/// leaves `app.follow` free, so `/channels <id> --follow` still works while a
/// rail is up.
pub struct FleetRail {
    fleet: String,
    channel_id: String,
    /// Channel log size at the last poll; unchanged size means nothing to parse.
    last_len: u64,
    /// Newest mtime seen in the jobs dir; jobs move far slower than events.
    last_jobs_mtime: Option<std::time::SystemTime>,
    view: RailView,
    next_poll: Instant,
}

impl FleetRail {
    pub fn start(home: &Path, fleet: &str) -> Self {
        let _ = home;
        Self {
            fleet: fleet.to_string(),
            channel_id: format!("fleet-{fleet}"),
            last_len: u64::MAX, // force the first poll to do real work
            last_jobs_mtime: None,
            view: RailView::default(),
            next_poll: Instant::now(),
        }
    }

    pub fn view(&self) -> &RailView {
        &self.view
    }

    pub fn next_poll(&self) -> Instant {
        self.next_poll
    }

    /// Recompute if anything moved. Returns true when the view changed, so the
    /// caller redraws only then.
    pub fn poll(&mut self, home: &Path, now: Instant) -> bool {
        self.next_poll = now + POLL_INTERVAL;
        let store = mur_channel::ChannelStore::new(home);
        let len = std::fs::metadata(store.events_path(&self.channel_id))
            .map(|m| m.len())
            .unwrap_or(0);
        let jobs_mtime = newest_jobs_mtime(home, &self.fleet);
        if len == self.last_len && jobs_mtime == self.last_jobs_mtime {
            return false;
        }
        self.last_len = len;
        self.last_jobs_mtime = jobs_mtime;

        let mut notice = None;
        let events = match store.load_events(&self.channel_id) {
            Ok(evs) => {
                // Same trust rule as every other fold: an event that fails its
                // actor's signature is dropped, never rendered. The rail
                // vouches for OTHER agents, so showing an unverified "done"
                // would lend the UI's credibility to a forgery.
                let require_sig = std::env::var("MUR_CHANNEL_REQUIRE_SIG")
                    .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"))
                    .unwrap_or(false);
                evs.into_iter()
                    .filter(|e| {
                        crate::channel_verify::verify_event(home, &self.channel_id, e, require_sig)
                    })
                    .collect::<Vec<_>>()
            }
            Err(_) => {
                notice = Some("⚠ channel unreadable".to_string());
                Vec::new()
            }
        };

        let jobs = crate::cmd::fleet::jobs::list_jobs_raw(home, &self.fleet).unwrap_or_default();
        let view = RailView {
            jobs_line: jobs_line(&self.fleet, &jobs),
            members: fold_members(&events),
            notice,
        };
        let changed = view != self.view;
        self.view = view;
        changed
    }
}

/// Newest mtime across the fleet's job files — the cheap "did jobs move?" gate.
fn newest_jobs_mtime(home: &Path, fleet: &str) -> Option<std::time::SystemTime> {
    let dir = crate::cmd::fleet::jobs::jobs_dir(home, fleet);
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}
```

- [ ] **Step 4: Add the non-reconciling job reader**

`list_jobs()` reloads the whole channel to reconcile (`jobs.rs:140`), so calling it here would parse the same log twice a second. Add a sibling in `mur-core/src/cmd/fleet/jobs.rs` that only reads the store, and make `list_jobs` use it so there is one reader:

```rust
/// Jobs straight from the store, oldest-first, with no channel reconciliation.
/// `list_jobs` adds reconciliation on top; callers that already hold the
/// channel's events (the murmur fleet rail) use this to avoid re-parsing it.
pub(crate) fn list_jobs_raw(mur_home: &Path, fleet: &str) -> Result<Vec<Job>> {
    // …body of today's list_jobs, up to and including the sort, minus the
    // reconcile_jobs call…
}
```

and `list_jobs` becomes:

```rust
pub fn list_jobs(mur_home: &Path, fleet: &str) -> Result<Vec<Job>> {
    let mut jobs = list_jobs_raw(mur_home, fleet)?;
    reconcile_jobs(mur_home, fleet, &mut jobs);
    Ok(jobs)
}
```

- [ ] **Step 5: Run the tests**

```bash
cargo nextest run -p mur-core --lib fleet_rail
cargo nextest run -p mur-core --lib fleet::jobs
```
Expected: all PASS — the second command proves the `list_jobs` split did not change its behavior.

- [ ] **Step 6: Commit**

```bash
git branch --show-current
git add mur-core/src/cmd/agent/cli/fleet_rail.rs mur-core/src/cmd/fleet/jobs.rs
git commit -m "feat(murmur): gated fleet-rail poller over channel + job store"
```

---

### Task 5: render the band, and keep the flush capacity honest

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/ui.rs`

**Interfaces:**
- Consumes: `RailView`, `MemberState`, `MAX_EXPANDED_ROWS`, `App.fleet` (Task 6 adds the field — for this task, add it first as `pub fleet: Option<super::fleet_rail::FleetRail>` in `app.rs` and leave it `None`).
- Produces: `pub fn fleet_rail_height(app: &App) -> u16`, `fn render_fleet_rail(f: &mut Frame, app: &App, area: Rect)`.

- [ ] **Step 1: Write the failing tests**

Add a test module to `ui.rs`:

```rust
#[cfg(test)]
mod fleet_rail_layout_tests {
    use super::*;
    use crate::cmd::agent::cli::fleet_rail::{MemberRow, MemberState, RailView};

    fn view(blocked: usize) -> RailView {
        RailView {
            jobs_line: "fleet · dev   job 0/1".into(),
            members: (0..blocked)
                .map(|i| MemberRow {
                    agent: format!("m{i}"),
                    state: MemberState::Blocked {
                        summary: "approve".into(),
                        hitl_id: format!("h{i}"),
                    },
                })
                .collect(),
            notice: None,
        }
    }

    #[test]
    fn rail_is_one_row_until_someone_is_blocked() {
        assert_eq!(rail_height_for(&view(0)), 1);
        assert_eq!(rail_height_for(&view(1)), 2);
        assert_eq!(rail_height_for(&view(3)), 4);
    }

    #[test]
    fn the_expanded_rail_is_capped() {
        use crate::cmd::agent::cli::fleet_rail::MAX_EXPANDED_ROWS;
        assert_eq!(
            rail_height_for(&view(50)),
            1 + MAX_EXPANDED_ROWS as u16,
            "an unbounded rail would eat the transcript"
        );
    }

    #[test]
    fn the_live_band_gives_back_exactly_what_the_rail_takes() {
        // The guard for the one dangerous coupling: band_inner_rows decides
        // when transcript content is flushed to scrollback, so it must account
        // for every row the rail paints or the flush drifts from the picture.
        let viewport_h = 20u16;
        let tmp = tempfile::TempDir::new().unwrap();
        // App::new(home, agent, session, theme) — the same constructor
        // `cmd_cli` uses (mod.rs:393). There is no App::for_test; the only
        // `for_test` in app.rs builds a ChatMsg.
        let mut app = App::new(
            tmp.path().to_path_buf(),
            "mur".to_string(),
            crate::cmd::agent::cli::persist::Session::create(tmp.path(), "mur").unwrap(),
            crate::cmd::agent::cli::theme::resolve_skin("dark"),
        );
        let without = band_inner_rows(&app, viewport_h);
        app.fleet_view_for_test = Some(view(3));
        let with = band_inner_rows(&app, viewport_h);
        assert_eq!(without - with, rail_height_for(&view(3)));
    }
}
```

- [ ] **Step 2: Run and watch it fail**

```bash
cargo nextest run -p mur-core --lib fleet_rail_layout
```
Expected: FAIL — `cannot find function rail_height_for`.

- [ ] **Step 3: Implement height + render + the subtraction**

In `ui.rs`:

```rust
/// Rows the fleet rail paints: one collapsed line, plus a capped member list
/// when someone is blocked. A working fleet is not news; a stalled one is.
pub fn rail_height_for(view: &crate::cmd::agent::cli::fleet_rail::RailView) -> u16 {
    use crate::cmd::agent::cli::fleet_rail::{MAX_EXPANDED_ROWS, MemberState};
    let blocked = view
        .members
        .iter()
        .any(|m| matches!(m.state, MemberState::Blocked { .. }));
    if !blocked {
        return 1;
    }
    1 + view.members.len().min(MAX_EXPANDED_ROWS) as u16
}

/// Height of the rail band for the current app state; 0 when `--fleet` is off.
pub fn fleet_rail_height(app: &App) -> u16 {
    app.fleet_view().map(rail_height_for).unwrap_or(0)
}

fn render_fleet_rail(f: &mut Frame, app: &App, area: Rect) {
    use crate::cmd::agent::cli::fleet_rail::{MAX_EXPANDED_ROWS, MemberState};
    let Some(view) = app.fleet_view() else { return };
    let theme = app.theme;
    let mut lines: Vec<Line> = Vec::new();

    let head = match &view.notice {
        Some(n) => format!("{}  {n}", view.jobs_line),
        None => view.jobs_line.clone(),
    };
    lines.push(Line::styled(
        head,
        Style::default().fg(theme.border_title).add_modifier(Modifier::BOLD),
    ));

    if rail_height_for(view) > 1 {
        for m in view.members.iter().take(MAX_EXPANDED_ROWS) {
            let (glyph, body, color) = match &m.state {
                MemberState::Blocked { summary, .. } => ("▲", format!("blocked: {summary}"), theme.warn),
                MemberState::Working { tool, since } => (
                    "⏵",
                    match tool {
                        Some(t) => format!("working ({}) · {t}", elapsed(*since)),
                        None => format!("working ({})", elapsed(*since)),
                    },
                    theme.agent,
                ),
                MemberState::Done => ("✔", "done".to_string(), theme.success),
                MemberState::Failed => ("✖", "failed".to_string(), theme.error),
            };
            lines.push(Line::styled(
                format!("  {:<10} {glyph} {body}", m.agent),
                Style::default().fg(color),
            ));
        }
        let extra = view.members.len().saturating_sub(MAX_EXPANDED_ROWS);
        if extra > 0 {
            lines.push(Line::styled(
                format!("  … {extra} more"),
                Style::default().fg(theme.system),
            ));
        }
    }

    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// "2m" / "1h04m" — elapsed since a member last changed state. Shown instead
/// of a staleness verdict: a runtime that died mid-turn shows a growing
/// number rather than a state we guessed.
fn elapsed(since: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - since).num_seconds().max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}
```

Then wire the band into `render()` (`ui.rs:51`), between the transcript and the chooser:

```rust
    let rail_h = fleet_rail_height(app);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(rail_h),
            Constraint::Length(chooser_h),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(f.area());

    render_transcript(f, app, chunks[0]);
    if rail_h > 0 {
        render_fleet_rail(f, app, chunks[1]);
    }
    if chooser_h > 0 {
        render_chooser_band(f, app, chunks[2]);
    } else {
        render_completion(f, app, chunks[3]);
    }
    f.render_widget(&app.input, chunks[3]);
    render_status(f, app, chunks[4]);
```

and subtract it in `band_inner_rows`:

```rust
fn band_inner_rows(app: &App, viewport_h: u16) -> u16 {
    let input_h = (app.input.lines().len() as u16 + 2).clamp(3, 8);
    let chooser_h = chooser_band_height(app, viewport_h, input_h);
    // The rail steals rows from the live band; miss it here and the flush
    // decision drifts from what is painted.
    let rail_h = fleet_rail_height(app);
    viewport_h.saturating_sub(input_h + 1 + chooser_h + rail_h + 2)
}
```

- [ ] **Step 4: Add the test seams to `App`**

In `app.rs`:

```rust
    /// Fleet rail, when `--fleet` is on. `None` for an ordinary murmur.
    pub fleet: Option<super::fleet_rail::FleetRail>,
    /// Test-only override so layout tests need no poller or filesystem.
    #[cfg(test)]
    pub fleet_view_for_test: Option<super::fleet_rail::RailView>,
```

```rust
impl App {
    /// The rail's current view, from the live poller or (in tests) the override.
    pub fn fleet_view(&self) -> Option<&super::fleet_rail::RailView> {
        #[cfg(test)]
        if self.fleet_view_for_test.is_some() {
            return self.fleet_view_for_test.as_ref();
        }
        self.fleet.as_ref().map(|f| f.view())
    }
}
```

Initialize `fleet: None` (and `fleet_view_for_test: None`) wherever `App` is constructed.

- [ ] **Step 5: Run the tests**

```bash
cargo nextest run -p mur-core --lib fleet_rail_layout
cargo nextest run -p mur-core --lib cmd::agent::cli
```
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git branch --show-current
git add mur-core/src/cmd/agent/cli/ui.rs mur-core/src/cmd/agent/cli/app.rs
git commit -m "feat(murmur): render the fleet rail and charge it to the band capacity"
```

---

### Task 6: poll it from the event loop, and verify it live

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (construct the rail; poll it in the loop)

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Construct the rail after `App` is built**

In `cmd_cli`, where the single-agent `App` is created:

```rust
    if let Some(f) = fleet.as_deref() {
        app.fleet = Some(super::cli::fleet_rail::FleetRail::start(&home, f));
    }
```

- [ ] **Step 2: Poll it on the same tick as `follow`**

In the event loop, next to the existing follow drain, before the draw:

```rust
        // Fleet rail: cheap when nothing moved (two metadata calls), and only
        // forces a redraw when the folded view actually changed.
        if let Some(rail) = app.fleet.as_mut()
            && StdInstant::now() >= rail.next_poll()
            && rail.poll(&app.home.clone(), StdInstant::now())
        {
            app.needs_full_redraw = true;
        }
```

- [ ] **Step 3: Build a release binary**

```bash
cargo build --release -p mur-core --bin mur
```
Expected: `Finished`.

- [ ] **Step 4: Verify against a real fleet in tmux**

```bash
# a fleet whose channel has events; create one if needed:
#   ./target/release/mur fleet create dev --members qa,backend
tmux kill-session -t railtest 2>/dev/null
tmux new-session -d -s railtest -x 100 -y 40 \
  "$PWD/target/release/mur agent cli mur --fleet dev"
sleep 12
tmux capture-pane -t railtest -p | tail -12
```
Expected: one `fleet · dev …` line directly above the composer; composer still on the bottom rows; status line last.

- [ ] **Step 5: Verify the blocked path expands**

Append a HitlRequest to the fleet channel from a second shell, then re-capture:

```bash
tmux capture-pane -t railtest -p | tail -14
```
Expected: the rail grows to two rows — the job line plus `backend  ▲ blocked: …` — and the transcript band shrinks by exactly one row. Kill the session: `tmux kill-session -t railtest`.

- [ ] **Step 6: Verify the unknown-fleet error**

```bash
./target/release/mur agent cli mur --fleet nope 2>&1 | head -2
```
Expected: a non-zero exit naming `--fleet nope`; no TUI opens.

- [ ] **Step 7: Full test + lint gate**

```bash
cargo nextest run -p mur-core --lib
cargo clippy -p mur-core --bin mur -- -D warnings
cargo fmt -p mur-core
```
Expected: all PASS, clippy clean.

- [ ] **Step 8: Commit and open the PR**

```bash
git branch --show-current
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(murmur): poll the fleet rail from the event loop"
git push -u origin HEAD
gh pr create --title "feat(murmur): --fleet status rail" --body "…"
```

---

## Self-Review

**Spec coverage**

| spec section | task |
|---|---|
| §1 surface: `--fleet` flag, unknown fleet is a hard error | 1 |
| §1 collapsed job line, `2/5` = terminal/total, empty state | 3 |
| §1 expanded on blocked, all members sorted blocked-first | 2 (fold + sort), 5 (height + render) |
| §2 state derivation table, Human/System excluded | 2 |
| §3 `FleetRail` separate from `Follow`, 700 ms gated poll, load-once | 4 |
| §3 `band_inner_rows` must subtract the rail | 5 (impl + guard test) |
| §4 unreadable channel degrades | 4 |
| §4 signature verification never bypassed | 4 |
| §4 elapsed time instead of a staleness verdict | 5 (`elapsed()`) |
| §4 cap at K=6 plus "… N more" | 5 |
| §4 corrupt jobs → fall back, no crash | 4 (`unwrap_or_default`) |
| §5 fold table test, height test, consistency guard | 2, 5 |

No gaps.

**Type consistency** — `MemberState` / `MemberRow` / `RailView` / `fold_members` / `jobs_line` / `MAX_EXPANDED_ROWS` / `rail_height_for` / `fleet_rail_height` / `App::fleet_view()` are spelled identically in every task that uses them. `list_jobs_raw` is introduced in Task 4 and used only there.

**Known follow-ups (out of scope, per the spec's "deliberately not in v1")** — approving a blocked member from the rail; more than one fleet per session; a pane per member.
