# Plan: the runtime loop switch — caps become deadline / stuck, inherited from the fleet

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-12-execution-limits-design.md` §3.2, §3.3, §3.4, §3.5, §3.7, §5, §6, §7; §9 step 4.
> Base: `main` after #1273 (step 3 — `mur_common::limits`, the `limits:` block on all three scopes, `Fleet::limits_or_legacy`, `mur limits`). This is the step that **changes what governs a run**: after it, no iteration cap and no token budget stops work; a deadline and a stuck clock do, and only when nobody is watching.

**Goal.** Every unattended turn and fleet loop is bounded by a resolved `deadline` and `stuck` (and `cost_usd` where billable), the old caps are loaded-and-ignored with one warning, and a task delegated by a fleet inherits the fleet's *remaining* deadline.

**Architecture.** `mur-agent-runtime` gains `bounds.rs`: `TurnBounds` (resolved per turn from `config.yaml limits:` → `profile.yaml limits:` → the caller's `limits.deadline_secs` A2A parameter, via `mur_common::limits::resolve`) and the attended/unattended split (`TaskSpec.attended`, set explicitly at every construction site). The agentic loop checks deadline and a progress clock instead of `max_iterations`/`max_token_budget`; the iteration counter survives as a diagnostic with a 10 000 ceiling. In `mur-core`, the fleet loop resolves the same schema (`Scope::FleetRun`), replaces the iteration-count stuck guard with a clock, and threads `deadline_at` through `DagExecOptions` so `channel/delegate` carries `limits.deadline_secs = remaining`. The daemon's eligibility check becomes "the fleet's limits resolve" (the built-in deadline makes every fleet bounded — §3.3 — so the gate now catches only a broken `limits:` block; the opt-in env and kill-switch are untouched).

**Tech stack.** Rust 2024, `cargo nextest`. `mur-core` env:
`ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`;
from a worktree add `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target`.

## Global Constraints (from the spec)

- §3.2: attended = a murmur session holds the task (the fact `can_approve` already carries). Attended: `deadline` ignored, `stuck` **warns** in the live band and never stops, `cost_usd` ignored, hard stop is `Esc`. Unattended: `deadline` required to start (built-in default if none set), `stuck` stops with `Stuck`, `cost_usd` enforced when present and billable.
- §3.3: built-in defaults — deadline 1h fleet run / 30m single task (unattended only), stuck 10m (warn attended, stop unattended), no cost cap. Constants live in `mur_common::limits` (already there).
- §3.4: the scope that launched the unit of work owns the budget. A task delegated with 12 minutes left on the fleet clock gets a 12-minute deadline, not a fresh one; no per-task cap inside a fleet run.
- §3.5: progress = a file write through any tool, a channel event authored by an agent, or a tool call whose `(name, arguments)` differ from the previous iteration's set. NOT progress: a text-only LLM turn, a tool call identical to the last one. Unattended stop reason carries the last three tool calls.
- §3.7: every stop reason reaches the settlement card with a one-line remedy; `finished` is reserved for a natural end.
- §5: opt-in `MUR_FLEET_AUTORUN=1`, kill-switch `.stopped`, governance fail-closed, `yes:false` everywhere — **unchanged**. "Bounded" is now satisfied by the resolved deadline.
- §6: `hitl.max_iterations` / `hitl.max_tokens` / `loop.max_iterations`: loaded, ignored, reported once at start and by `mur limits` ("IGNORED since 2.79 — remove it"). Never an error. The internal iteration counter stays as a diagnostic with ceiling 10 000; it is not a setting.
- **`TaskSpec` has no `Default` on purpose** — every construction site must state `attended` explicitly, the way it already states `intent`. Do not add a `Default` impl to get past the compiler.
- Changing a `pub fn` signature or a `mur-common` struct that the workspace-excluded Hub uses breaks the Hub: grep `mur-hub-gui/src-tauri/src` for the name, run the Hub `cargo check` **last** (symlink `mur-hub-gui/ui/dist` from the main checkout if the worktree lacks it; remove the symlink before committing).
- Before every commit: `cargo fmt`, `cargo clippy -p <crate> --all-targets -- -D warnings` (read the exit code, not the grep), the named tests green.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-agent-runtime/src/bounds.rs` (new) | `TurnBounds`, `Progress` clock, `bounds_for` resolution, last-three-calls formatting; tests | 1, 2 |
| `mur-agent-runtime/src/lib.rs` | `pub mod bounds;` | 1 |
| `mur-agent-runtime/src/task_runner.rs` | `TaskSpec { attended, deadline_secs }`, `limits` field + `with_limits`, loop guards, `LoopStop::{Deadline,Stuck}`, ceiling, drop token budget | 1, 2 |
| `mur-agent-runtime/src/protocol/methods/message_send.rs`, `channel_delegate.rs` | parse `limits.deadline_secs`; set `attended` | 1 |
| `mur-agent-runtime/src/scheduler.rs`, `idle_scheduler.rs`, `watch_scheduler.rs` | `attended: false, deadline_secs: None` | 1 |
| `mur-agent-runtime/src/turn_ledger.rs` | `StopKind::{Deadline, Stuck}`, `remedy(agent)` returns `String`, ceiling wording | 2 |
| `mur-agent-runtime/src/supervisor.rs`, `supervisor_runner.rs` | stop passing caps; `with_limits(config, profile)`; warn-once on stale keys | 3 |
| `mur-core/src/cmd/fleet/loop_run.rs` | resolver-driven deadline/stuck/cost, clock-based stuck, ceiling, `deadline_at`, remedies, ignored-flag notices | 4 |
| `mur-core/src/cmd/fleet/run.rs` | `deadline_at` on the one-shot run | 4 |
| `mur-core/src/executor/dag.rs` | `DagExecOptions.deadline_at`; `channel/delegate` params carry `limits.deadline_secs` | 4 |
| `mur-daemon/src/fleet_tick.rs` | eligibility = limits resolve; tests rewritten | 4 |
| `mur-core/src/cmd/fleet/billing.rs` | delete `is_bounded` / `unbounded_reason` (superseded) | 4 |
| `mur-core/src/cmd/limits.rs` | `STEP4_NOTE` → "IGNORED since 2.79 — remove it" | 4 |
| `CLAUDE.md`, `mur-common/src/agent.rs` (HitlConfig docs), `mur-core/src/cli/actions.rs` (`set-loop` help) | say what is true now | 5 |

---

## Task 1 — `TurnBounds`: what bounds this turn, and is anyone watching

**Interfaces.**
- Consumes: `mur_common::limits::{Limits, Scope, Stuck, resolve, Resolved}` (step 3).
- Produces:

```rust
// mur_agent_runtime::bounds
pub struct TurnBounds { pub attended: bool, pub deadline: Option<std::time::Instant>, pub stuck: Stuck, pub deadline_source: Source }
pub fn resolve_bounds(attended: bool, global: &Limits, agent: Option<&Limits>, caller_deadline_secs: Option<u64>, now: std::time::Instant) -> Result<TurnBounds, String>
// TaskSpec gains
pub attended: bool,          // false = nobody can answer / stop this turn by hand
pub deadline_secs: Option<u64>,  // the caller's remaining clock (fleet delegation), None = resolve from scopes
// TaskRunner gains
pub fn with_limits(self, global: Limits, agent: Option<Limits>) -> Self
```

### Steps

- [ ] **1.1 Write the failing tests** — new file `mur-agent-runtime/src/bounds.rs`:

```rust
//! What bounds one turn (spec 2026-09-12 execution-limits §3.2–§3.5).
//!
//! Resolution is `mur_common::limits::resolve` over the scopes the runtime can
//! see — `config.yaml` and the agent's own profile — plus the caller's
//! remaining clock as the innermost layer, so a task delegated by a fleet with
//! twelve minutes left gets twelve minutes, not a fresh half hour (§3.4). The
//! attended split is decided here and nowhere else: a turn somebody is
//! watching has no deadline and a stuck clock that only warns.

use std::time::{Duration, Instant};

use mur_common::limits::{Limits, Scope, Source, Stuck, resolve};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnBounds {
    pub attended: bool,
    /// `None` when attended, or when a scope set an unparsable value that
    /// validation already rejected upstream (never reached in practice).
    pub deadline: Option<Instant>,
    pub stuck: Stuck,
    pub deadline_source: Source,
}

/// `caller_deadline_secs` is the innermost scope: the remaining time the
/// launching fleet has. It rides the `Flag` layer of the resolver because that
/// is exactly what it is — a per-call value that beats every file.
pub fn resolve_bounds(
    attended: bool,
    global: &Limits,
    agent: Option<&Limits>,
    caller_deadline_secs: Option<u64>,
    now: Instant,
) -> Result<TurnBounds, String> {
    let flags = Limits {
        deadline: caller_deadline_secs.map(|s| format!("{s}s")),
        stuck: None,
        cost_usd: None,
    };
    let r = resolve(Scope::SingleTask, global, None, agent, &flags)?;
    Ok(TurnBounds {
        attended,
        deadline: if attended { None } else { r.deadline.value.map(|d| now + d) },
        stuck: r.stuck.value,
        deadline_source: r.deadline.source,
    })
}

/// The stuck clock. `note_progress` resets it; `stuck_for` is how long the
/// turn has gone without a progress signal.
#[derive(Debug, Clone)]
pub struct Progress {
    last: Instant,
    /// The previous iteration's `(tool, args-fingerprint)` set — a call that
    /// repeats one of these is not progress (§3.5).
    prev_calls: Vec<(String, u64)>,
    /// Last three calls, newest last, for the stop reason.
    recent: std::collections::VecDeque<String>,
}

impl Progress {
    pub fn start(now: Instant) -> Self {
        Self {
            last: now,
            prev_calls: Vec::new(),
            recent: std::collections::VecDeque::with_capacity(3),
        }
    }

    /// Feed one iteration's tool calls. Progress iff at least one call wrote a
    /// file or differs from every call of the previous iteration. A text-only
    /// iteration (`calls` empty) is never progress.
    pub fn observe(&mut self, calls: &[(String, u64)], now: Instant) {
        let progressed = calls.iter().any(|(tool, fp)| {
            is_file_write(tool) || !self.prev_calls.iter().any(|(t, f)| t == tool && f == fp)
        });
        for (tool, _) in calls {
            if self.recent.len() == 3 {
                self.recent.pop_front();
            }
            self.recent.push_back(tool.clone());
        }
        if progressed {
            self.last = now;
        }
        self.prev_calls = calls.to_vec();
    }

    pub fn stuck_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.last)
    }

    /// `write_file, bash, bash` — what the stop reason shows.
    pub fn last_calls(&self) -> String {
        if self.recent.is_empty() {
            "no tool calls".to_string()
        } else {
            self.recent.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    }
}

/// The tools that write files, by name. Kept as a list here rather than a
/// trait method because the fleet loop's channel-event rule (§3.5) is the
/// other half of "progress" and lives in mur-core; both sides stay data.
fn is_file_write(tool: &str) -> bool {
    matches!(tool, "write_file" | "edit_file" | "append_file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(deadline: Option<&str>, stuck: Option<&str>) -> Limits {
        Limits {
            deadline: deadline.map(str::to_string),
            stuck: stuck.map(str::to_string),
            cost_usd: None,
        }
    }

    /// §3.2: attended has no deadline whatever the files say; unattended gets
    /// the narrowest scope, and the caller's remaining clock beats the files.
    #[test]
    fn attended_has_no_deadline_and_the_callers_clock_wins_unattended() {
        let now = Instant::now();
        let global = l(Some("4h"), Some("20m"));
        let agent = l(Some("45m"), None);
        let a = resolve_bounds(true, &global, Some(&agent), Some(720), now).unwrap();
        assert_eq!(a.deadline, None);
        assert_eq!(a.stuck, Stuck::After(Duration::from_secs(20 * 60)), "stuck still resolves — it warns");
        let u = resolve_bounds(false, &global, Some(&agent), Some(720), now).unwrap();
        assert_eq!(u.deadline, Some(now + Duration::from_secs(720)), "the fleet's remaining twelve minutes");
        assert_eq!(u.deadline_source, Source::Flag);
        let u = resolve_bounds(false, &global, Some(&agent), None, now).unwrap();
        assert_eq!(u.deadline, Some(now + Duration::from_secs(45 * 60)));
        assert_eq!(u.deadline_source, Source::Agent);
        let u = resolve_bounds(false, &Limits::default(), None, None, now).unwrap();
        assert_eq!(u.deadline, Some(now + mur_common::limits::DEFAULT_DEADLINE_TASK));
        assert_eq!(u.deadline_source, Source::BuiltIn);
    }

    /// §3.5: identical calls are not progress; a file write always is; a
    /// text-only iteration never is.
    #[test]
    fn progress_resets_only_on_new_calls_or_file_writes() {
        let t0 = Instant::now();
        let mut p = Progress::start(t0);
        let bash = ("bash".to_string(), 1u64);
        p.observe(std::slice::from_ref(&bash), t0 + Duration::from_secs(10));
        assert_eq!(p.stuck_for(t0 + Duration::from_secs(10)), Duration::ZERO, "first call is new");
        p.observe(std::slice::from_ref(&bash), t0 + Duration::from_secs(20));
        assert_eq!(p.stuck_for(t0 + Duration::from_secs(20)), Duration::from_secs(10), "same call again: clock keeps running");
        p.observe(&[], t0 + Duration::from_secs(30));
        assert_eq!(p.stuck_for(t0 + Duration::from_secs(30)), Duration::from_secs(20), "text-only turn: not progress");
        p.observe(&[("write_file".to_string(), 1)], t0 + Duration::from_secs(40));
        assert_eq!(p.stuck_for(t0 + Duration::from_secs(40)), Duration::ZERO, "a file write is always progress");
        p.observe(&[("write_file".to_string(), 1)], t0 + Duration::from_secs(50));
        assert_eq!(p.stuck_for(t0 + Duration::from_secs(50)), Duration::ZERO, "even the same write again");
        assert_eq!(p.last_calls(), "bash, write_file, write_file");
    }
}
```

- [ ] **1.2 Register and run** — add `pub mod bounds;` to `mur-agent-runtime/src/lib.rs` (alphabetical). `cargo nextest run -p mur-agent-runtime --lib -E 'test(/bounds::/)'`. Expected: 2 pass.

- [ ] **1.3 Write the failing `TaskSpec` test** — in `mur-agent-runtime/src/protocol/methods/message_send.rs` tests (create `#[cfg(test)] mod limits_params` at the bottom if the file has no test module):

```rust
#[cfg(test)]
mod limits_params {
    /// The wire shape both handlers read: `limits.deadline_secs` is the
    /// caller's remaining clock; absent means "resolve from the scopes".
    #[test]
    fn deadline_secs_is_read_from_limits_and_absent_is_none() {
        let p = serde_json::json!({"limits": {"deadline_secs": 720}});
        assert_eq!(super::caller_deadline_secs(&p), Some(720));
        assert_eq!(super::caller_deadline_secs(&serde_json::json!({})), None);
        assert_eq!(super::caller_deadline_secs(&serde_json::json!({"limits": {"deadline_secs": -1}})), None);
    }
}
```

- [ ] **1.4 Watch it fail** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(deadline_secs_is_read)'`. Expected: `cannot find function caller_deadline_secs`.

- [ ] **1.5 Add the fields and the parser** —

  `mur-agent-runtime/src/task_runner.rs`, in `pub struct TaskSpec`, after `pub output_artifact_path: ...` (last field):

```rust
    /// Is a person watching this turn and able to stop it by hand? Attended
    /// turns have no deadline and a stuck clock that only warns (spec §3.2).
    /// Deliberately not defaulted: every construction site says which it is,
    /// the way it already says `intent`. `message/send` passes its
    /// `can_approve`; `channel/delegate` and every runtime scheduler pass
    /// `false`.
    pub attended: bool,
    /// The launching scope's REMAINING clock, in seconds — a fleet delegating
    /// with twelve minutes left passes 720 (spec §3.4). `None` = resolve the
    /// deadline from `profile.yaml` → `config.yaml` → built-in.
    pub deadline_secs: Option<u64>,
```

  `mur-agent-runtime/src/protocol/methods/message_send.rs`, a free function above the handler impl:

```rust
/// `params.limits.deadline_secs`, when the caller sent one. Negative or
/// non-integer values read as absent — the resolver then applies the scopes,
/// which is the safe direction (a bound, not none).
pub(crate) fn caller_deadline_secs(p: &Value) -> Option<u64> {
    p.get("limits")
        .and_then(|l| l.get("deadline_secs"))
        .and_then(|v| v.as_u64())
}
```

  In the same handler, in the `TaskSpec { .. }` literal add `attended: can_approve, deadline_secs: caller_deadline_secs(&p),`. In `channel_delegate.rs`'s literal add `attended: false, deadline_secs: super::message_send::caller_deadline_secs(&p),`. In `scheduler.rs` (2 sites), `idle_scheduler.rs`, `watch_scheduler.rs` add `attended: false, deadline_secs: None,`. In `task_runner.rs` tests (11 sites) add `attended: true, deadline_secs: None,` — the existing tests model interactive turns; Task 2 adds the unattended ones.

  `TaskRunner` struct: add field `limits: (mur_common::limits::Limits, Option<mur_common::limits::Limits>),` after `hitl_timeout_secs`, initialised `limits: (Default::default(), None),` in the constructor, and the builder next to `with_hitl_timeout_secs`:

```rust
    /// The two scopes the runtime can see: `config.yaml limits:` and the
    /// agent's own `profile.yaml limits:`. Resolved per turn in `bounds_for`.
    pub fn with_limits(
        mut self,
        global: mur_common::limits::Limits,
        agent: Option<mur_common::limits::Limits>,
    ) -> Self {
        self.limits = (global, agent);
        self
    }

    /// What bounds this turn. Errors only when a file carries an unparsable
    /// value — surfaced as a task failure that names the key, per spec §4.
    fn bounds_for(&self, spec: &TaskSpec) -> Result<crate::bounds::TurnBounds, TaskError> {
        crate::bounds::resolve_bounds(
            spec.attended,
            &self.limits.0,
            self.limits.1.as_ref(),
            spec.deadline_secs,
            std::time::Instant::now(),
        )
        .map_err(|e| TaskError::Internal(format!("limits: {e}")))
    }
```

  If `TaskError` has no `Internal` variant, use the variant the file already uses for configuration failures (grep `enum TaskError` first; do not add a variant).

- [ ] **1.6 Watch it pass** — `cargo check -p mur-agent-runtime --all-targets` until zero `E0063`; then `cargo nextest run -p mur-agent-runtime --lib -E 'test(/bounds::|deadline_secs_is_read/)'`. Expected: 3 pass. `bounds_for` is unused until Task 2 — put `#[allow(dead_code)]` on it with the comment `// consumed by Task 2 of the loop-switch plan` and remove the attribute there.

- [ ] **1.7 fmt + clippy on `mur-agent-runtime`**, then **commit**:

```
feat(runtime): TurnBounds — attended split and the caller's remaining clock

TaskSpec now says whether a person is watching (`attended`, set explicitly
at every construction site) and carries the launching scope's remaining
deadline (`deadline_secs`, read from the A2A `limits` parameter). The
resolver reuses mur_common::limits with the caller's clock as the innermost
layer, so a task delegated with twelve minutes left gets twelve minutes.
The Progress clock defines what counts as progress (§3.5). Nothing reads
the bounds yet.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 2 — the loop: deadline and stuck govern; iterations and tokens do not

**Interfaces.**
- Consumes: `TurnBounds`, `Progress`, `bounds_for` (Task 1); existing `fingerprint_args`, `graceful_exit`, `LoopExit`, `settle`, `TurnLedger`.
- Produces: `LoopStop::{Deadline, Stuck}` (runtime-private enum) and `StopKind::{Deadline, Stuck}` (public, serialised in ledgers); `StopKind::remedy(&self, agent: &str) -> Option<String>`; `ITERATION_CEILING: u32 = 10_000`; `LoopStop::TokenBudget` and `max_token_budget` removed; usage JSON `stop_reason` ∈ {`deadline`, `stuck`, `loop_detected`, `iteration_ceiling`}; a live-band warning `⚠ no progress for 10m — Esc to stop` on attended turns (once).

### Steps

- [ ] **2.1 Write the failing tests** — in `mur-agent-runtime/src/task_runner.rs` tests, next to `max_iterations_exceeded_yields_completed_with_summary`. Use the same stub client + tool helpers that test uses (read it first and copy its construction verbatim; the helpers below name what they must do):

```rust
    /// §7: an attended run passes the old 25-iteration mark without stopping.
    /// The stub returns a fresh tool call every turn and ends on turn 30.
    #[tokio::test]
    async fn attended_run_passes_the_old_iteration_cap() {
        // stub: 30 iterations of `bash` with distinct args, then end_turn
        let (runner, spec) = runner_with_scripted_tool_calls(30, /*attended*/ true, /*stuck*/ "off");
        let out = runner.run_sync(spec).await;
        let usage = task_usage(&out);
        assert!(usage.get("stop_reason").is_none(), "must end naturally: {usage}");
        assert_eq!(usage["iterations"], 30);
    }

    /// §7: unattended, three identical text-only turns with a zero stuck clock
    /// stop with `stuck` and name the last calls; the same run with a file
    /// write each turn does not stop.
    #[tokio::test]
    async fn unattended_stuck_stops_on_no_progress_and_not_after_a_write() {
        let (runner, spec) = runner_with_scripted_tool_calls_repeating("bash", 5, false, "0s");
        let out = runner.run_sync(spec).await;
        let usage = task_usage(&out);
        assert_eq!(usage["stop_reason"], "stuck", "{usage}");
        let card = last_agent_text(&out);
        assert!(card.contains("bash, bash"), "last calls named: {card}");
        assert!(card.contains("mur limits") && card.contains("--stuck"), "remedy: {card}");

        let (runner, spec) = runner_with_scripted_tool_calls_repeating("write_file", 5, false, "0s");
        let out = runner.run_sync(spec).await;
        let usage = task_usage(&out);
        assert_ne!(usage.get("stop_reason").and_then(|v| v.as_str()), Some("stuck"), "{usage}");
    }

    /// §3.2 + §3.7: an unattended turn past its deadline stops with `deadline`
    /// and the remedy names the agent's limits command.
    #[tokio::test]
    async fn unattended_deadline_stops_with_reason_and_remedy() {
        let (runner, mut spec) = runner_with_scripted_tool_calls(50, false, "off");
        spec.deadline_secs = Some(0); // already expired
        let out = runner.run_sync(spec).await;
        let usage = task_usage(&out);
        assert_eq!(usage["stop_reason"], "deadline", "{usage}");
        assert!(last_agent_text(&out).contains("mur limits"), "{}", last_agent_text(&out));
    }

    /// §6: the ceiling is a diagnostic, not a setting — it trips only at 10k.
    #[test]
    fn iteration_ceiling_is_absurd_on_purpose() {
        assert_eq!(ITERATION_CEILING, 10_000);
    }
```

  Add the three helpers in the test module. `runner_with_scripted_tool_calls(n, attended, stuck)` builds a `TaskRunner` with a stub LLM that emits, for iteration `i < n`, one `bash` tool call with input `{"cmd": format!("step {i}")}`, and `end_turn` at `n`; registers a no-op `bash` tool that returns `"ok"`; calls `.with_limits(Limits { stuck: Some(stuck.into()), ..Default::default() }, None)`; returns a `TaskSpec { attended, deadline_secs: None, intent: RequestIntent::Interactive, .. }`. `runner_with_scripted_tool_calls_repeating(tool, n, attended, stuck)` is the same but every iteration emits the same `tool` with the same input `{"path": "x"}` (`write_file` stub returns `"ok"` too). `task_usage(&out)` returns `out.task.usage.clone().unwrap_or_default()` (adapt to the `TaskOutcome` shape the existing test reads `usage` from). `last_agent_text(&out)` joins the last agent message's text parts. Put them next to the existing stub-client helpers so they share the stub types; do not invent a second stub.

- [ ] **2.2 Watch them fail** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(/attended_run_passes|unattended_stuck|unattended_deadline|iteration_ceiling/)'`. Expected: compile errors (`ITERATION_CEILING`, `with_limits` OK from Task 1; helpers missing until you add them; then the loop stops at 25 for the attended test).

- [ ] **2.3 Switch the loop** — in `mur-agent-runtime/src/task_runner.rs`:

  Constants (replace `DEFAULT_MAX_ITERATIONS` / `DEFAULT_MAX_TOKEN_BUDGET` and their docs):

```rust
/// Diagnostic ceiling on agentic-loop iterations (spec §6). Not a setting:
/// it exists so a runaway bug becomes a stop with a reason instead of a hang.
/// Bounds that a user configures are `deadline` and `stuck` (`mur limits`).
pub const ITERATION_CEILING: u32 = 10_000;
```

  `LoopStop` (runtime-private):

```rust
enum LoopStop {
    /// `ITERATION_CEILING` — a runaway, not a budget.
    IterationCeiling,
    LoopDetected,
    Deadline,
    Stuck,
}

impl LoopStop {
    fn as_str(self) -> &'static str {
        match self {
            LoopStop::IterationCeiling => "iteration_ceiling",
            LoopStop::LoopDetected => "loop_detected",
            LoopStop::Deadline => "deadline",
            LoopStop::Stuck => "stuck",
        }
    }
}
```

  Struct: delete `max_token_budget` and `with_max_token_budget`; rename field `max_iterations` → `iteration_ceiling: u32` (default `ITERATION_CEILING`), keep `with_max_iterations` but make it `#[cfg(test)] pub(crate) fn with_iteration_ceiling(mut self, n: u32)` and rewrite the ~15 test call sites to the new name (they cap stub loops; production never calls it). Delete `DEFAULT_MAX_TOKEN_BUDGET`, `cumulative_input_tokens` snapshot logic that only served the budget (`start_tokens` and the `spent >=` block) — keep the counters themselves, the usage report reads them.

  `run_agentic_loop` gains a parameter `bounds: crate::bounds::TurnBounds` (pass from `run_sync`'s call site as `self.bounds_for(&spec)?` computed **before** the match so the error surfaces as a failed task, not a panic). Inside, replace the loop head:

```rust
        let mut progress = crate::bounds::Progress::start(std::time::Instant::now());
        // The last three calls travel in the stuck reason; the ledger travels
        // in the card. Warn once on attended turns; there is no second warning
        // because the person is the stop.
        let mut stuck_warned = false;
        let mut iteration: u32 = 0;
        while iteration < self.iteration_ceiling {
            let now = std::time::Instant::now();
            if let Some(d) = bounds.deadline
                && now >= d
            {
                let msg = self
                    .graceful_exit(client, &history, LoopStop::Deadline, &ledger, iteration, &progress)
                    .await;
                return Ok((msg, Some(LoopExit { reason: LoopStop::Deadline, iterations: iteration })));
            }
            if let mur_common::limits::Stuck::After(limit) = bounds.stuck
                && progress.stuck_for(now) >= limit
                && iteration > 0
            {
                if bounds.attended {
                    if !stuck_warned {
                        stuck_warned = true;
                        self.emit_live_warning(task_id, &format!(
                            "⚠ no progress for {} — Esc to stop",
                            crate::bounds::fmt_dur(limit)
                        )).await;
                    }
                } else {
                    let msg = self
                        .graceful_exit(client, &history, LoopStop::Stuck, &ledger, iteration, &progress)
                        .await;
                    return Ok((msg, Some(LoopExit { reason: LoopStop::Stuck, iterations: iteration })));
                }
            }
```

  and after the tool results are appended each iteration (just before `iteration += 1;`):

```rust
            progress.observe(
                &resp
                    .tool_calls
                    .iter()
                    .map(|c| (c.tool_name.clone(), fingerprint_args(&c.input)))
                    .collect::<Vec<_>>(),
                std::time::Instant::now(),
            );
```

  The text-only branch that ends the turn (`resp.tool_calls.is_empty() || EndTurn`) returns before this point today — that is correct: a natural end is not "stuck". The loop's tail (after `while`) uses `LoopStop::IterationCeiling`.

  `emit_live_warning(task_id, text)`: send `{"kind":"warning","text":text}` on the task's registered client notifier if one exists (look up `self.client_notifiers` by `task_id`, ignore send errors). murmur already renders `warning`-kind frames in the live band (grep `"warning"` in `mur-core/src/agent_cli/` to confirm the key; if the band reads a different key, use that one and say so in the commit).

  Add `pub fn fmt_dur(d: Duration) -> String` to `bounds.rs` (`10m`, `2h`, `45s` — copy `fmt_dur` from `mur-core/src/cmd/limits.rs` verbatim so both print the same way).

  `graceful_exit` gains `progress: &crate::bounds::Progress` and its nudge becomes reason-specific:

```rust
        let why = match reason {
            LoopStop::Deadline => "the deadline for this task has passed".to_string(),
            LoopStop::Stuck => format!("no progress was made — the last tool calls were: {}", progress.last_calls()),
            LoopStop::LoopDetected => "the last tool call repeated with identical arguments and output".to_string(),
            LoopStop::IterationCeiling => format!("the {ITERATION_CEILING}-iteration safety ceiling was hit"),
        };
        let nudge = format!(
            "Stop calling tools: {why}. Summarize what you completed, the current \
             build/test state, and the remaining steps so work can resume later."
        );
```

  and its ledger mapping:

```rust
            stop: match reason {
                LoopStop::IterationCeiling => crate::turn_ledger::StopKind::MaxIterations,
                LoopStop::LoopDetected => crate::turn_ledger::StopKind::LoopDetected,
                LoopStop::Deadline => crate::turn_ledger::StopKind::Deadline,
                LoopStop::Stuck => crate::turn_ledger::StopKind::Stuck { last_calls: progress.last_calls() },
            },
```

  `sanitize_dangling_tool_uses(history, reason)` takes a `LoopStop` — update its match arms for the new variants (it only uses the reason for a string; keep that).

- [ ] **2.4 `StopKind` grows and its remedy names the agent** — `mur-agent-runtime/src/turn_ledger.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopKind {
    /// The model finished on its own.
    EndTurn,
    /// The 10 000-iteration diagnostic ceiling (spec §6) — a runaway.
    MaxIterations,
    /// Retained so ledgers written before 2.79 still deserialise. Never
    /// produced: the token budget is gone.
    TokenBudget,
    LoopDetected,
    /// Output hit `max_tokens` mid-thought.
    MaxTokens,
    /// The unattended deadline passed (spec §3.2).
    Deadline,
    /// No progress for the stuck window; the last three tool calls (§3.5).
    Stuck { last_calls: String },
}
```

  `StopKind` was `Copy`; it no longer can be. Change `is_clean(self)` / `as_str(self)` to `&self`, fix the borrow sites clippy names (there are few — the ledger is cloned once at settle). `as_str`: `Deadline => "deadline"`, `Stuck { .. } => "stuck"`, `MaxIterations => "iteration ceiling"`.

```rust
    /// What to do about it. The remedy names the command that exists NOW —
    /// `mur limits <agent>` — because the settlement card is where the user
    /// learns which bound bit.
    pub fn remedy(&self, agent: &str) -> Option<String> {
        Some(match self {
            StopKind::EndTurn => return None,
            StopKind::MaxIterations => format!(
                "{ITERATION_CEILING_NOTE} — this is a runaway, not a setting; report it with the transcript"
            ),
            StopKind::TokenBudget => "an old token budget stopped this turn; upgrade the agent runtime".to_string(),
            StopKind::LoopDetected => {
                "the last tool call repeated with identical arguments — change the ask, or the tool's input".to_string()
            }
            StopKind::MaxTokens => "the model's output limit — ask it to continue from where it stopped".to_string(),
            StopKind::Deadline => format!("raise it: mur limits {agent} --deadline <1h>  (or --deadline for the fleet that launched it)"),
            StopKind::Stuck { last_calls } => format!(
                "no progress; last calls: {last_calls} — change the ask, or widen it: mur limits {agent} --stuck <20m|off>"
            ),
        })
    }
```

  with `const ITERATION_CEILING_NOTE: &str = "the 10000-iteration safety ceiling was hit";` at the top of the file (the runtime constant lives in `task_runner.rs`; the ledger module must not import the runner, so the number is repeated here with a test that pins them equal — add `assert!(ITERATION_CEILING_NOTE.contains(&crate::task_runner::ITERATION_CEILING.to_string()))` to the existing remedy test).

  Update the one production caller `ledger.stop.remedy()` (line ~393, inside the card renderer) to `ledger.stop.remedy(agent)` — the renderer needs the agent name: thread it as a parameter of `settle`/the render fn from `TaskRunner.agent_name` (grep `fn settle(` and its callers; there are two — `graceful_exit` and the natural-end path). Update the four tests at lines ~716–733 to pass `"dev"` and to cover `Deadline` and `Stuck`:

```rust
        assert!(StopKind::Deadline.remedy("dev").unwrap().contains("mur limits dev --deadline"));
        let s = StopKind::Stuck { last_calls: "bash, bash, bash".into() }.remedy("dev").unwrap();
        assert!(s.contains("bash, bash, bash") && s.contains("--stuck"), "{s}");
```

- [ ] **2.5 Usage JSON** — `usage_obj["stop_reason"] = exit.reason.as_str().into();` already picks up the new strings. Remove the `"token_budget"` expectation from any test that asserted it (grep `token_budget` in tests; the test `..._with_max_token_budget(10)` at ~4113 tests a removed feature — delete it and say so in the commit).

- [ ] **2.6 Watch it pass** — `cargo nextest run -p mur-agent-runtime --lib`. Expected: all pass, including the four new tests. If `attended_run_passes_the_old_iteration_cap` is slow (30 stub turns), it should still be < 2 s; if not, the stub is doing I/O — fix the stub, not the assertion.

- [ ] **2.7 fmt + clippy on `mur-agent-runtime`**, then **commit**:

```
feat(runtime): deadline and stuck govern the loop; iterations and tokens do not

An attended turn has no deadline and a stuck clock that warns once in the
live band; an unattended turn stops on its resolved deadline or after the
stuck window with no progress — where progress is a file write or a tool
call that differs from the previous iteration's (§3.5). The iteration
counter stays as a diagnostic with a 10 000 ceiling (§6); the token budget
is gone. Every stop lands in the settlement with a remedy that names
`mur limits <agent>`. Deleted the token-budget test with the feature.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 3 — the supervisor: pass the limits, ignore the caps, say so once

**Interfaces.**
- Consumes: `TaskRunner::with_limits` (Task 1); `AgentProfile.limits`, `Config.limits` (step 3); `HitlConfig.max_iterations/max_tokens` (legacy).
- Produces: `build_provider_runner` loses its `max_iterations: Option<u32>, max_tokens: Option<u64>` parameters and gains `limits: (Limits, Option<Limits>)`; `pub fn stale_cap_warnings(hitl: &HitlConfig) -> Vec<String>` in `supervisor.rs` (pure, tested).

### Steps

- [ ] **3.1 Write the failing test** — `mur-agent-runtime/src/supervisor.rs` tests:

```rust
    /// §6: a profile with the old caps starts, warns once per key, and the
    /// value has no effect (the runner no longer has a setter to receive it).
    #[test]
    fn stale_caps_warn_once_and_name_the_replacement() {
        let mut h = mur_common::agent::HitlConfig::default();
        assert!(stale_cap_warnings(&h).is_empty());
        h.max_iterations = Some(800);
        h.max_tokens = Some(1_000_000);
        let w = stale_cap_warnings(&h);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("hitl.max_iterations: 800") && w[0].contains("IGNORED since 2.79"), "{}", w[0]);
        assert!(w[1].contains("hitl.max_tokens") && w[1].contains("mur limits"), "{}", w[1]);
    }
```

- [ ] **3.2 Watch it fail** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(stale_caps_warn_once)'`. Expected: `cannot find function stale_cap_warnings`.

- [ ] **3.3 Implement** — `supervisor.rs`, near the call site (~line 559):

```rust
/// §6: the old per-agent caps are loaded and ignored. One line per key at
/// start, never an error — nobody's agent stops starting over a stale key.
pub fn stale_cap_warnings(hitl: &mur_common::agent::HitlConfig) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(n) = hitl.max_iterations {
        out.push(format!(
            "profile.yaml hitl.max_iterations: {n} — IGNORED since 2.79; remove it (bounds are `mur limits <agent>`: deadline / stuck)"
        ));
    }
    if let Some(n) = hitl.max_tokens {
        out.push(format!(
            "profile.yaml hitl.max_tokens: {n} — IGNORED since 2.79; remove it (bounds are `mur limits <agent>`: deadline / stuck / cost_usd)"
        ));
    }
    out
}
```

  Replace

```rust
    let max_iterations = profile.inner.hitl.max_iterations;
    let max_tokens = profile.inner.hitl.max_tokens;
```

  with

```rust
    for line in stale_cap_warnings(&profile.inner.hitl) {
        tracing::warn!(agent = %profile.inner.name, "{line}");
        eprintln!("warning: {line}");
    }
    // The two scopes this process can see (spec §3.4). The fleet's clock, when
    // there is one, arrives per turn in the A2A `limits` parameter.
    let limits = (
        mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).limits,
        profile.inner.limits.clone(),
    );
```

  (`mur_home` is in scope in `supervisor.rs` at that point — grep to confirm the binding name; it is the same value `supervisor_runner.rs:331` derives from `MUR_HOME`.) Pass `limits` where `max_iterations, max_tokens` were passed. In `supervisor_runner.rs::build_provider_runner` and the inner `build_runner` it forwards to, replace the two parameters with `limits: (mur_common::limits::Limits, Option<mur_common::limits::Limits>)`, delete the two `if let Some(n) = …` blocks, and add `runner = runner.with_limits(limits.0, limits.1);`. Fix the test at ~4234 (`build_runner_caps_loop_at_configured_max_iterations`) — it proves a removed feature; replace it with:

```rust
    /// The supervisor's runner is built with the profile's limits, proven by
    /// an unattended deadline of zero stopping the first iteration.
    #[tokio::test]
    async fn build_runner_applies_profile_limits() {
        // build via build_runner(...) with limits (Limits::default(), Some(Limits { deadline: Some("0s".into()), ..Default::default() }))
        // run an unattended TaskSpec against the stub; assert usage["stop_reason"] == "deadline"
    }
```

  written out in full against the same stub the deleted test used.

- [ ] **3.4 Watch it pass** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(/stale_caps|build_runner_applies/)'` then the whole crate. `command grep -rn "build_provider_runner\|max_token_budget\|with_max_iterations" mur-gui-core/src mur-hub-gui/src-tauri/src` must print nothing.

- [ ] **3.5 fmt + clippy**, then **commit**:

```
feat(runtime): the supervisor passes limits, ignores the old caps, and says so once

hitl.max_iterations / hitl.max_tokens are loaded and ignored: one warning
per key at agent start (tracing + stderr), never an error. The runner is
built with config.yaml's and the profile's limits: blocks instead.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 4 — the fleet loop: the same schema, a clock for stuck, and the remaining deadline handed down

**Interfaces.**
- Consumes: `mur_common::limits::{resolve, Scope, Stuck, Source}`, `Fleet::limits_or_legacy`, `Config.limits` (step 3); `fleet::billing::{fleet_billing, FleetBilling}` (step 2); `budget_for`, `no_cap_notice`, `emit_stop_event`, `stop_remedy` (steps 1–2).
- Produces: `DagExecOptions.deadline_at: Option<std::time::Instant>`; `build_channel_delegate_params(text, cid, child_task_id, key, deadline_secs: Option<u64>)`; `check_guards(iteration, elapsed, deadline, stuck_for, stuck) -> Option<LoopStop>` (new signature); `pub fn fleet_bounds(mur_home, fleet, flag_deadline: Option<&str>, flag_budget: Option<f64>) -> anyhow::Result<FleetBounds>` in `loop_run.rs` with `pub struct FleetBounds { pub deadline: Duration, pub deadline_source: Source, pub stuck: Stuck, pub cost_usd: Option<f64> }`; `LOOP_ITERATION_CEILING: u32 = 10_000`; daemon `eligible(mur_home, fleet) -> Result<(), String>`.

### Steps

- [ ] **4.1 Write the failing tests** — `mur-core/src/cmd/fleet/loop_run.rs` tests. Replace `check_guards_precedence_and_trips` and `effective_max_iterations_precedence` (both test removed behaviour) with:

```rust
    /// Guards, narrowest reason first: deadline, then stuck, then the
    /// diagnostic ceiling. Stuck is a CLOCK now — how long since progress —
    /// not a count of iterations.
    #[test]
    fn guards_are_deadline_then_stuck_then_ceiling() {
        use mur_common::limits::Stuck;
        let m = Duration::from_secs(60);
        assert_eq!(check_guards(0, 0 * m, 60 * m, 0 * m, Stuck::After(10 * m)), None);
        assert_eq!(check_guards(3, 61 * m, 60 * m, 0 * m, Stuck::After(10 * m)), Some(LoopStop::Deadline));
        assert_eq!(check_guards(3, 5 * m, 60 * m, 10 * m, Stuck::After(10 * m)), Some(LoopStop::Stuck));
        assert_eq!(check_guards(3, 5 * m, 60 * m, 99 * m, Stuck::Off), None, "off never trips");
        assert_eq!(check_guards(LOOP_ITERATION_CEILING, 5 * m, 60 * m, 0 * m, Stuck::Off), Some(LoopStop::MaxIterations));
        // deadline beats stuck when both are due
        assert_eq!(check_guards(3, 61 * m, 60 * m, 10 * m, Stuck::After(10 * m)), Some(LoopStop::Deadline));
    }

    /// §3.3 + §3.4: a fleet with no limits: block gets the 1h built-in;
    /// flags beat fleet.yaml beats config.yaml; a legacy loop.budget_usd is
    /// the cost cap only on a billable fleet.
    #[test]
    fn fleet_bounds_resolve_across_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("config.yaml"), "limits:\n  deadline: 4h\n  stuck: 20m\n").unwrap();
        let mut f = fleet_named("dev"); // the fixture the file already has; add `limits: None`
        f.loop_cfg = Some(FleetLoop { trigger: "manual".into(), max_iterations: 8, budget_usd: 5.0, deadline: "2h".into(), done_when: String::new() });
        let b = fleet_bounds(home, &f, None, None).unwrap();
        assert_eq!(b.deadline, Duration::from_secs(2 * 3600));
        assert_eq!(b.deadline_source, mur_common::limits::Source::Fleet);
        assert_eq!(b.stuck, mur_common::limits::Stuck::After(Duration::from_secs(20 * 60)));
        // no models.yaml → billing unknown → billable → the legacy budget applies
        assert_eq!(b.cost_usd, Some(5.0));
        let b = fleet_bounds(home, &f, Some("15m"), Some(1.0)).unwrap();
        assert_eq!(b.deadline, Duration::from_secs(15 * 60));
        assert_eq!(b.deadline_source, mur_common::limits::Source::Flag);
        assert_eq!(b.cost_usd, Some(1.0));
        f.loop_cfg = None;
        std::fs::remove_file(home.join("config.yaml")).unwrap();
        let b = fleet_bounds(home, &f, None, None).unwrap();
        assert_eq!(b.deadline, mur_common::limits::DEFAULT_DEADLINE_FLEET);
        assert_eq!(b.deadline_source, mur_common::limits::Source::BuiltIn);
        // an unparsable value is an error that names the key, not a default
        f.limits = Some(mur_common::limits::Limits { deadline: Some("soon".into()), stuck: None, cost_usd: None });
        let e = fleet_bounds(home, &f, None, None).unwrap_err().to_string();
        assert!(e.contains("limits.deadline") && e.contains("soon"), "{e}");
    }

    /// The remedies name the commands that exist now.
    #[test]
    fn remedies_name_mur_fleet_limits() {
        assert!(stop_remedy(LoopStop::Deadline, "dev").unwrap().contains("mur fleet limits dev --deadline"));
        assert!(stop_remedy(LoopStop::Stuck, "dev").unwrap().contains("--stuck"));
        assert!(stop_remedy(LoopStop::Budget, "dev").unwrap().contains("--cost-usd"));
        assert!(stop_remedy(LoopStop::MaxIterations, "dev").unwrap().contains("ceiling"));
    }
```

  and in `mur-core/src/executor/dag.rs` tests:

```rust
    /// §3.4: the delegate carries the fleet's REMAINING clock, never a fresh one.
    #[test]
    fn delegate_params_carry_remaining_deadline() {
        let p = build_channel_delegate_params("do x", "fleet-dev", "t1", "k1", Some(720));
        assert_eq!(p["limits"]["deadline_secs"], 720);
        let p = build_channel_delegate_params("do x", "fleet-dev", "t1", "k1", None);
        assert!(p.get("limits").is_none(), "no clock → the member resolves its own scopes");
    }

    #[test]
    fn remaining_secs_floors_at_one_and_is_none_without_a_deadline() {
        let now = std::time::Instant::now();
        assert_eq!(remaining_secs(None, now), None);
        assert_eq!(remaining_secs(Some(now + std::time::Duration::from_secs(90)), now), Some(90));
        assert_eq!(remaining_secs(Some(now), now), Some(1), "a delegate that starts at the deadline gets one second, not zero");
    }
```

  and in `mur-daemon/src/fleet_tick.rs` tests, replacing `eligibility_is_bounded_not_budgeted` and `due_fleets_requires_positive_budget`:

```rust
    /// §5 after the loop switch: every fleet is bounded by its resolved
    /// deadline (built-in 1h when nothing is set), so a local fleet with no
    /// knobs auto-runs; a fleet whose limits: block does not parse never does;
    /// the kill-switch still wins (tested below, unchanged).
    #[test]
    fn eligibility_is_a_resolvable_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let f = loop_fleet("plain", "interval:1m"); // no deadline, no budget
        store::save_fleet(home, &f).unwrap();
        assert_eq!(due_fleets(home, 5000).unwrap(), vec!["plain".to_string()]);
        let mut bad = loop_fleet("bad", "interval:1m");
        bad.limits = Some(mur_common::limits::Limits { deadline: Some("soon".into()), stuck: None, cost_usd: None });
        store::save_fleet(home, &bad).unwrap();
        let due = due_fleets(home, 5000).unwrap();
        assert!(!due.contains(&"bad".to_string()), "unparsable limits never auto-run: {due:?}");
    }
```

  (`loop_fleet` is the existing fixture; make its `budget_usd` 0.0 and `deadline` empty if it is not already, and add `limits: None`.)

- [ ] **4.2 Watch them fail** — `cargo nextest run -p mur-core --lib -E 'test(/guards_are_deadline|fleet_bounds_resolve|remedies_name|delegate_params_carry|remaining_secs_floors/)'` and `cargo nextest run -p mur-daemon -E 'test(eligibility_is_a_resolvable)'`. Expected: compile errors for the new names.

- [ ] **4.3 `loop_run.rs`** —

  Replace `DEFAULT_MAX_ITERATIONS`, `STUCK_LIMIT`, `effective_max_iterations`, `effective_deadline`, `effective_budget` with:

```rust
/// Diagnostic ceiling on loop iterations (spec §6). Not a setting — the bounds
/// a user sets are `deadline`, `stuck` and `cost_usd` (`mur fleet limits`).
pub const LOOP_ITERATION_CEILING: u32 = 10_000;

/// What bounds this fleet run, resolved across `--flags` → `fleet.yaml
/// limits:` (or its legacy `loop.*`) → `config.yaml limits:` → built-in.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetBounds {
    pub deadline: Duration,
    pub deadline_source: mur_common::limits::Source,
    pub stuck: mur_common::limits::Stuck,
    /// The configured cost cap, before `budget_for` decides whether the
    /// fleet can spend at all.
    pub cost_usd: Option<f64>,
}

pub fn fleet_bounds(
    mur_home: &Path,
    fleet: &Fleet,
    flag_deadline: Option<&str>,
    flag_budget: Option<f64>,
) -> Result<FleetBounds> {
    let global = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).limits;
    let fleet_limits = fleet.limits_or_legacy();
    let flags = mur_common::limits::Limits {
        deadline: flag_deadline.map(|d| d.trim().to_string()),
        stuck: None,
        cost_usd: flag_budget,
    };
    let r = mur_common::limits::resolve(
        mur_common::limits::Scope::FleetRun,
        &global,
        fleet_limits.as_ref(),
        None,
        &flags,
    )
    .map_err(|e| anyhow::anyhow!("fleet '{}': {e}", fleet.name))?;
    Ok(FleetBounds {
        // FleetRun always resolves a deadline (built-in when nothing is set);
        // the Option exists for the SingleTask/attended case.
        deadline: r.deadline.value.unwrap_or(mur_common::limits::DEFAULT_DEADLINE_FLEET),
        deadline_source: r.deadline.source,
        stuck: r.stuck.value,
        cost_usd: r.cost_usd.value,
    })
}

/// Pure pre-iteration guard check. `stuck_for` = time since the last
/// agent-authored channel event. Deadline first: when both are due, the
/// clock that the user set explicitly is the one to name.
pub fn check_guards(
    iteration: u32,
    elapsed: Duration,
    deadline: Duration,
    stuck_for: Duration,
    stuck: mur_common::limits::Stuck,
) -> Option<LoopStop> {
    if elapsed >= deadline {
        return Some(LoopStop::Deadline);
    }
    if let mur_common::limits::Stuck::After(limit) = stuck
        && stuck_for >= limit
        && iteration > 0
    {
        return Some(LoopStop::Stuck);
    }
    if iteration >= LOOP_ITERATION_CEILING {
        return Some(LoopStop::MaxIterations);
    }
    None
}
```

  In `run_guarded`: replace the `max_iter`/`deadline`/`configured_budget` resolution with

```rust
    if max_iterations.is_some() {
        println!("  ℹ --max-iterations is ignored since 2.79 — the bounds are deadline / stuck / cost_usd (mur fleet limits {name})");
    }
    if fleet.loop_cfg.as_ref().is_some_and(|l| l.max_iterations != 0) {
        println!("  ℹ fleet.yaml loop.max_iterations is IGNORED since 2.79 — remove it (mur fleet limits {name})");
    }
    let bounds = fleet_bounds(mur_home, &fleet, deadline.as_deref(), budget_usd)?;
    let deadline = Some(bounds.deadline);
    println!(
        "  bounds: deadline {} ← {}; stuck {}",
        crate::cmd::limits::fmt_dur(bounds.deadline),
        bounds.deadline_source.label(),
        match bounds.stuck {
            mur_common::limits::Stuck::Off => "off".to_string(),
            mur_common::limits::Stuck::After(d) => crate::cmd::limits::fmt_dur(d),
        }
    );
    let billing = super::billing::fleet_billing(mur_home, &fleet);
    let configured_budget = bounds.cost_usd.filter(|&b| b > 0.0);
    let budget = budget_for(configured_budget, &billing);
```

  (make `fmt_dur` in `cmd/limits.rs` `pub(crate)`). Keep everything from `if configured_budget.is_some_and(...)` onward as it is — `no_cap_notice(&billing, budget, deadline, price_per_1k)` still takes `Option<Duration>`.

  Stuck becomes a clock: replace `let mut stuck = 0u32;` with `let mut last_progress = Instant::now();`; the guard call becomes `check_guards(iteration, start.elapsed(), bounds.deadline, last_progress.elapsed(), bounds.stuck)`; the post-iteration block becomes

```rust
        // Stuck-detection (§3.5, fleet half): an agent-authored channel event
        // is progress and resets the clock; a router-only iteration is not.
        let events = svc.load_events(&fleet.channel_id)?;
        let progressed = events
            .iter()
            .any(|e| e.seq > last_seq && matches!(e.actor, ChannelActor::Agent { .. }));
        last_seq = events.last().map(|e| e.seq).unwrap_or(last_seq);
        if progressed {
            last_progress = Instant::now();
        }
```

  Any other reader of the old `stuck` counter (grep `stuck` in the file — the iteration summary line and the `progress.json` writer may print it) prints `last_progress.elapsed()` formatted with `fmt_dur` instead.

  `DagExecOptions` in the loop: add `deadline_at: Some(start + bounds.deadline),`.

  `stop_remedy`:

```rust
        LoopStop::MaxIterations => format!(
            "the {LOOP_ITERATION_CEILING}-iteration safety ceiling — a runaway, not a setting; report it with the run id (mur fleet status {fleet})"
        ),
        LoopStop::Deadline => format!("raise it: mur fleet limits {fleet} --deadline <2h>  (fleet.yaml limits.deadline)"),
        LoopStop::Stuck => format!(
            "no member activity for the stuck window — see what they are waiting on: mur fleet status {fleet}; widen it: mur fleet limits {fleet} --stuck <20m|off>"
        ),
        LoopStop::Budget => format!("raise it: mur fleet limits {fleet} --cost-usd <USD>  (fleet.yaml limits.cost_usd)"),
```

  and update `every_stop_short_of_done_names_a_remedy`'s three `contains` assertions (`--deadline`, `--cost-usd`, and drop the `--max-iterations` one for `contains("ceiling")`).

- [ ] **4.4 `dag.rs`** — `DagExecOptions` gains

```rust
    /// The launching scope's deadline, as an instant (spec §3.4). A delegated
    /// member is handed the REMAINING seconds in `limits.deadline_secs`, so
    /// its clock is the fleet's, not a fresh one. `None` = the member resolves
    /// its own scopes (a one-shot `mur fleet run` without a deadline, a
    /// workflow step).
    pub deadline_at: Option<std::time::Instant>,
```

  (`Default` derive covers `None`). Add

```rust
/// Seconds left on the launching clock, floored at one so a delegate that
/// starts on the deadline is told to stop at once rather than told nothing.
fn remaining_secs(deadline_at: Option<std::time::Instant>, now: std::time::Instant) -> Option<u64> {
    deadline_at.map(|d| d.saturating_duration_since(now).as_secs().max(1))
}
```

  `build_channel_delegate_params` gains `deadline_secs: Option<u64>` and inserts `"limits": {"deadline_secs": n}` when `Some`. The call in `execute_step` passes `remaining_secs(opts.deadline_at, std::time::Instant::now())`.

- [ ] **4.5 `run.rs`** — the one-shot `mur fleet run` is unattended too: before building `opts`, `let bounds = super::loop_run::fleet_bounds(mur_home, &fleet, None, None)?;` and set `deadline_at: Some(std::time::Instant::now() + bounds.deadline),`. (`fleet_bounds` is `pub`; `run.rs` already imports from `loop_run`? It is the other way round — `loop_run` imports `run::build_fleet_procedure`; a `super::loop_run::` path from `run.rs` is fine, both are siblings under `cmd/fleet`.)

- [ ] **4.6 `fleet_tick.rs`** — replace `eligible(lc, &billing)` and the `unbounded_reason` warning with

```rust
        // §5 after the loop switch: bounded = the fleet's limits resolve. The
        // built-in deadline (1h) makes every fleet bounded; what this still
        // catches is a limits: block that does not parse — which must never
        // auto-run on a default it did not ask for.
        let bounded = match mur_core::cmd::fleet::loop_run::fleet_bounds(mur_home, &fleet, None, None) {
            Ok(_) => true,
            Err(e) => {
                if is_due(trigger, read_last_run(mur_home, &name), now_unix) {
                    tracing::warn!(fleet = %name, "fleet_tick: not auto-running — {e:#}");
                }
                false
            }
        };
```

  delete the local `fn eligible`, and delete `is_bounded` / `unbounded_reason` (and their tests) from `mur-core/src/cmd/fleet/billing.rs` — `command grep -rn "is_bounded\|unbounded_reason" mur-core mur-daemon` must print nothing afterwards. Keep `kill_switch_beats_a_bounded_due_fleet` (rename the doc line: "bounded by the built-in deadline").

- [ ] **4.7 `mur limits` stale note** — `mur-core/src/cmd/limits.rs`:

```rust
const STEP4_NOTE: &str = "IGNORED since 2.79 — remove it; the bounds are limits: deadline / stuck / cost_usd";
```

  and update `a_legacy_fleet_is_read_and_its_stale_keys_named`'s assertion from `"still applied until"` to `"IGNORED since 2.79"`.

- [ ] **4.8 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/fleet::|executor::dag::|cmd::limits/)'`, `cargo nextest run -p mur-daemon`. Then the deep-research path that passes `max_iterations: 4`: `command grep -rn "max_iterations" mur-core/src/cmd/deep_research/` — leave the field (it is `FleetLoop`'s), the loop now prints the ignored notice; confirm `cargo run -q -p mur-core --bin mur -- fleet run deep-research --loop --deadline 1s` (real home, harmless: it stops on the deadline before the first iteration) prints `bounds: deadline 1s ← command-line flag` and the two ℹ lines, then a `stopped: deadline` settlement.

- [ ] **4.9 fmt + clippy on `mur-core`, `mur-daemon`; Hub check last**, then **commit**:

```
feat(fleet): the loop resolves limits:, stuck is a clock, delegates inherit the remaining deadline

`mur fleet run --loop` resolves deadline / stuck / cost_usd across flags →
fleet.yaml limits: (or legacy loop.*) → config.yaml → built-in (1h), prints
the bound and its source, and ignores max_iterations with a notice (§6).
Stuck is time since the last agent-authored channel event, not an
iteration count (§3.5). Every channel/delegate carries the fleet's
REMAINING seconds as limits.deadline_secs (§3.4). The daemon's eligibility
is "the limits resolve" — the built-in deadline bounds every fleet, so the
gate now catches only a broken block; opt-in env and kill-switch unchanged.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 5 — say what is true now

**Interfaces.** Consumes everything above; produces text only.

### Steps

- [ ] **5.1 `CLAUDE.md`** — in the `mur fleet` bullet, replace the safety-triad sentence `auto-run also requires a positive \`loop.budget_usd\`` with `auto-run requires the fleet's \`limits:\` to resolve (every fleet is bounded by its deadline — built-in 1h — or a \`cost_usd\` on a billable model; see the 2026-09-12 execution-limits spec §5)`, and replace `run --loop adds guards (iteration cap, deadline, stuck-detection, --budget-usd from real per-token spend)` with `run --loop is bounded by \`deadline\` / \`stuck\` / \`cost_usd\` resolved across flags → fleet.yaml \`limits:\` → config.yaml → built-in (\`mur limits <fleet>\` shows what is in force and from where); iteration caps and token budgets are gone`. Add one line under the bullet: `- \`mur limits <name>\` / \`mur fleet limits\` / \`mur agent limits\` — show or edit the three knobs per scope; attended (murmur) turns have no hard stop, unattended ones stop on deadline or no progress.`
- [ ] **5.2 `HitlConfig` docs** (`mur-common/src/agent.rs`) — the two field docs become `/// IGNORED since 2.79 (kept so old profiles load; warned at agent start). Bounds live in \`limits:\` — see \`mur limits <agent>\`.`
- [ ] **5.3 `set-loop` help** (`mur-core/src/cli/actions.rs`) — `--max-iterations`: `Ignored since 2.79 (kept for old scripts); bounds are \`mur fleet limits\``; `--budget-usd`: `Legacy spelling of \`limits.cost_usd\` — prefer \`mur fleet limits <name> --cost-usd\``; `--deadline`: append `— prefer \`mur fleet limits <name> --deadline\``.
- [ ] **5.4 `mur verify --file CLAUDE.md`** must pass (it scans for stale command claims). Commit:

```
docs: the bounds are deadline / stuck / cost_usd — CLAUDE.md, HitlConfig, set-loop help

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## After the last task

- One PR: `feat: runtime loop switch — deadline/stuck govern, caps ignored, fleet deadline inherited (spec step 4)`. The PR body must say plainly: **behaviour change** — unattended runs that used to stop at 25 iterations / 750k tokens now stop at 30m (single task) or 1h (fleet) unless `limits:` says otherwise; attended murmur turns no longer stop on their own.
- Real-machine checks after `build.sh --install` and `mur update --restart-agents` (agents must restart to pick up the new runtime — `running.lock` build_sha is the judge, not `--version`): (1) `murmur` a 30-step coding ask — no stop, a `⚠ no progress` warning only if it idles; (2) `mur agent send dr_worker_1 "..."` with `limits: {deadline: 1s}` in its profile → settlement `stopped: deadline`; (3) `mur fleet run develop-rust --loop --deadline 5m` → `bounds:` line, delegates' `mur limits` show `← command-line flag`; (4) an agent with `hitl.max_iterations` still set → one `warning:` line in its `/tmp` launchd log at start and it runs past 25.
- Release note for 2.79.0 (the `mur-release` skill): the behaviour change above, the two ignored keys, the new commands.
- `update-docs`: README fleet paragraph + `limits` page; product page card "bounded, not budgeted".
- Next steps in the spec: 5 (heartbeats + handle-returning tools), 6 (dispatch preflight + `ToolError::NotAuthorized`). Hub 3b (`LimitsPanel`) can start any time — it wraps `cmd::limits::report`.
