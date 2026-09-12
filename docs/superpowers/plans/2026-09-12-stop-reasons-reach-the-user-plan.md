# Plan: stop reasons reach the settlement card and the fleet rail

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-12-execution-limits-design.md` §3.7 and §9 step 1.
> Base: `main` at or after `b30221c8` (2.78.0). No schema change; nothing here
> depends on the `limits:` work in later steps.

**Goal.** When a fleet loop or a single task stops short, the user sees *why*
and *what to do*, in the transcript they are already reading — never a bare
`finished (0s)` or an empty turn.

**Architecture.** The fleet loop already knows its `LoopStop` and writes it
only to `progress.json`; Task 1 also writes it as a signed System
`state-change` channel event carrying `stop_reason` and `remedy`. Task 2
folds that event in the fleet rail (`RailView.stop`) and renders it. Task 3
makes murmur's fleet headline say `stopped: <reason>` instead of
`finished`. Task 4 adds the remedy to the settlement card's existing
`⚠ stopped at …` row for single tasks, which the runtime already emits.

**Tech stack.** Rust 2024, `cargo nextest`. Env for `mur-core`:
`ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`.
Use `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target` from a
worktree to avoid a cold build.

## Global Constraints (from the spec)

- Every stop names its reason **and** its remedy, where the user is looking (settlement card, fleet rail, murmur headline). `progress.json` keeps `outcome` as today.
- The word `finished` is reserved for `Converged`.
- The stop event is written **signed as the channel's writer** through `crate::channel_writer::append_as_writer`, the same path the DAG uses; never an unsigned append when a key exists.
- No new user-facing setting. Remedies name the knobs that exist **today** (`mur fleet settings … --max-iterations/--deadline/--budget-usd`, `hitl.max_iterations`/`hitl.max_tokens` in the profile); step 3 of the rollout rewrites them.
- Before every commit: `cargo fmt -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings` clean, the named tests green. Any `std::os::unix::` in a test fixture sits under `#[cfg(unix)]`.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-core/src/cmd/fleet/loop_run.rs` | `stop_remedy()`, `terminal_state_for()`, `emit_stop_event()` called once at the end of `run_guarded`; tests | 1 |
| `mur-core/src/cmd/agent/cli/fleet_rail.rs` | `StopNotice`, `fold_stop()`, `RailView.stop`, `summary()` renders it | 2 |
| `mur-core/src/cmd/agent/cli/fleet_rail/tests.rs` | fold + summary tests | 2 |
| `mur-core/src/cmd/agent/cli/app.rs` | `finish_auto_fleet` headline uses the rail's stop | 3 |
| `mur-agent-runtime/src/turn_ledger.rs` | `StopKind::remedy()`, rendered on the `⚠ stopped at` row | 4 |

---

## Task 1 — the fleet loop writes its stop reason to the channel

**Interfaces.**
- Consumes: `LoopStop` (exists, 9 variants), `outcome_label(LoopStop) -> &'static str` (exists), `crate::channel_writer::append_as_writer(svc, home, channel_id, router_agent, actor, kind, payload, idem) -> anyhow::Result<ChannelEvent>` (exists), `Fleet::router_or_concierge()` (exists), `STUCK_LIMIT` (exists, 2).
- Produces: `pub fn stop_remedy(stop: LoopStop, fleet: &str) -> Option<String>`; `fn terminal_state_for(stop: LoopStop) -> &'static str`; a System `EventKind::StateChange` event at the end of every `run_guarded` with payload `{ "from": "working", "to": <terminal_state_for>, "stop_reason": <outcome_label>, "remedy": <stop_remedy or null>, "iterations": <u32>, "spent_usd": <f64>, "run_id": <String> }`.

### Steps

- [x] **1.1 Write the failing tests** — in `mur-core/src/cmd/fleet/loop_run.rs`, inside the existing `#[cfg(test)] mod tests`, right after `progress_file_written_with_outcome_on_guard_stop`:

```rust
    /// Every stop has a remedy except the two that mean "done". The remedy
    /// names a command that exists today; the spec's later steps rewrite it.
    #[test]
    fn every_stop_short_of_done_names_a_remedy() {
        for stop in [
            LoopStop::MaxIterations,
            LoopStop::Deadline,
            LoopStop::Stuck,
            LoopStop::Budget,
            LoopStop::Stopped,
            LoopStop::CommanderKilled,
            LoopStop::AwaitingApproval,
        ] {
            let r = stop_remedy(stop, "dev").unwrap_or_else(|| panic!("{stop:?} has no remedy"));
            assert!(r.contains("dev"), "{stop:?}: remedy must name the fleet: {r}");
        }
        assert_eq!(stop_remedy(LoopStop::Converged, "dev"), None);
        assert_eq!(stop_remedy(LoopStop::QueueDrained, "dev"), None);
        assert!(stop_remedy(LoopStop::MaxIterations, "dev").unwrap().contains("--max-iterations"));
        assert!(stop_remedy(LoopStop::Deadline, "dev").unwrap().contains("--deadline"));
        assert!(stop_remedy(LoopStop::Budget, "dev").unwrap().contains("--budget-usd"));
        assert!(stop_remedy(LoopStop::Stopped, "dev").unwrap().contains("mur fleet start dev"));
        assert!(stop_remedy(LoopStop::AwaitingApproval, "dev").unwrap().contains("mur channel approve"));
    }

    /// The map from "why we stopped" to the channel's terminal state. Done is
    /// completed; a kill is canceled; waiting on a person is input-required;
    /// a guard trip is failed — the goal was not reached.
    #[test]
    fn stop_maps_to_a_channel_terminal_state() {
        assert_eq!(terminal_state_for(LoopStop::Converged), "completed");
        assert_eq!(terminal_state_for(LoopStop::QueueDrained), "completed");
        assert_eq!(terminal_state_for(LoopStop::Stopped), "canceled");
        assert_eq!(terminal_state_for(LoopStop::CommanderKilled), "canceled");
        assert_eq!(terminal_state_for(LoopStop::AwaitingApproval), "input-required");
        for stop in [LoopStop::MaxIterations, LoopStop::Deadline, LoopStop::Stuck, LoopStop::Budget] {
            assert_eq!(terminal_state_for(stop), "failed", "{stop:?}");
        }
    }

    /// The stop reaches the channel, not only progress.json: one System
    /// state-change at the end of the run carrying reason and remedy.
    #[tokio::test]
    async fn a_guard_stop_is_written_to_the_channel_with_reason_and_remedy() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        let svc = mur_channel::ChannelService::open(home).unwrap();
        svc.create_for_fleet("dev", "mur", &["pm".into()]).unwrap();
        // Kill-switch: the loop stops before any delegation, no live agent needed.
        crate::cmd::fleet::control::cmd_fleet_stop(home, "dev").unwrap();

        let stop = run_loop_for_test(home).await;
        assert_eq!(stop, LoopStop::Stopped);

        let events = svc.load_events("fleet-dev").unwrap();
        let last = events.last().expect("the stop event is the last thing written");
        assert_eq!(last.kind, mur_common::channel::EventKind::StateChange);
        assert_eq!(last.actor, ChannelActor::System);
        assert_eq!(last.payload["to"], "canceled");
        assert_eq!(last.payload["stop_reason"], "stopped");
        assert_eq!(last.payload["remedy"], "cleared by: mur fleet start dev");
        assert!(last.payload["run_id"].as_str().is_some_and(|s| !s.is_empty()));
    }
```

- [x] **1.2 Watch it fail** — `cargo nextest run -p mur-core --lib -E 'test(/every_stop_short_of_done|stop_maps_to_a_channel|a_guard_stop_is_written/)'`. Expected: compile error `cannot find function stop_remedy` (and `terminal_state_for`).

- [x] **1.3 Add the two pure functions** — in `loop_run.rs` directly below `fn outcome_label`:

```rust
/// The one-line way out for each stop, in the words of the commands that exist
/// today (`mur fleet settings`, `mur fleet start`, `mur channel approve`). The
/// spec's step 3 rewrites these when `limits:` lands; until then a user who
/// hits a cap must at least be told which knob it was. `None` for the two
/// stops that mean the work is done.
pub fn stop_remedy(stop: LoopStop, fleet: &str) -> Option<String> {
    Some(match stop {
        LoopStop::Converged | LoopStop::QueueDrained => return None,
        LoopStop::MaxIterations => format!(
            "raise it: mur fleet settings {fleet} --max-iterations <N>  (fleet.yaml loop.max_iterations)"
        ),
        LoopStop::Deadline => format!(
            "raise it: mur fleet settings {fleet} --deadline <2h>  (fleet.yaml loop.deadline)"
        ),
        LoopStop::Stuck => format!(
            "no member activity for {STUCK_LIMIT} iterations — see what they are waiting on: mur fleet status {fleet}"
        ),
        LoopStop::Budget => format!(
            "raise it: mur fleet settings {fleet} --budget-usd <USD>  (fleet.yaml loop.budget_usd)"
        ),
        LoopStop::Stopped => format!("cleared by: mur fleet start {fleet}"),
        LoopStop::CommanderKilled => format!(
            "a commander directive halted {fleet} — inspect it: mur commander status"
        ),
        LoopStop::AwaitingApproval => format!(
            "a member is waiting on you: mur channel approve fleet-{fleet} <hitl_id>"
        ),
    })
}

/// The channel terminal state a stop implies, in the kebab-case wire form
/// `channel_terminal_status` and the rail already fold. Done is `completed`; a
/// kill is `canceled`; waiting on a person is `input-required`; a guard trip
/// is `failed`, because the goal was not reached — the run ending is not the
/// same as the work being done.
fn terminal_state_for(stop: LoopStop) -> &'static str {
    match stop {
        LoopStop::Converged | LoopStop::QueueDrained => "completed",
        LoopStop::Stopped | LoopStop::CommanderKilled => "canceled",
        LoopStop::AwaitingApproval => "input-required",
        LoopStop::MaxIterations | LoopStop::Deadline | LoopStop::Stuck | LoopStop::Budget => {
            "failed"
        }
    }
}
```

- [x] **1.4 Write the event at the end of `run_guarded`** — replace the block that begins `// Stamp the terminal state onto the progress file` and ends `Ok((stop, iteration, spent))` with:

```rust
    // Stamp the terminal state onto the progress file — kept as the last-run
    // record (overwritten by the next run). Best-effort.
    let run_id = {
        let mut g = lock_progress(&progress);
        g.finished_at = Some(chrono::Utc::now().to_rfc3339());
        g.outcome = Some(outcome_label(stop).to_string());
        g.iteration = iteration;
        g.spend_usd = spent;
        g.save(mur_home, name);
        g.run_id.clone()
    };
    // And onto the channel, where the rail, murmur and the Hub are looking.
    // Until this existed the reason lived only in progress.json, and every
    // surface said "finished" for a run that had hit a cap. Signed as the
    // writer like every other event this run wrote; best-effort like the
    // progress file — a stop must never fail because its announcement did.
    emit_stop_event(&svc, mur_home, &fleet, stop, iteration, spent, &run_id);
    Ok((stop, iteration, spent))
}

/// One System `state-change` carrying why the loop stopped and the way out.
/// `from` is always `working`: a loop that is ending was running.
fn emit_stop_event(
    svc: &mur_channel::ChannelService,
    mur_home: &Path,
    fleet: &Fleet,
    stop: LoopStop,
    iterations: u32,
    spent_usd: f64,
    run_id: &str,
) {
    let payload = serde_json::json!({
        "from": "working",
        "to": terminal_state_for(stop),
        "stop_reason": outcome_label(stop),
        "remedy": stop_remedy(stop, &fleet.name),
        "iterations": iterations,
        "spent_usd": spent_usd,
        "run_id": run_id,
    });
    if let Err(e) = crate::channel_writer::append_as_writer(
        svc,
        mur_home,
        &fleet.channel_id,
        fleet.router_or_concierge(),
        ChannelActor::System,
        mur_common::channel::EventKind::StateChange,
        payload,
        None,
    ) {
        tracing::warn!(fleet = %fleet.name, error = %e, "could not write the stop reason to the channel");
    }
}
```

  `svc` and `fleet` are already in scope in `run_guarded` (`let svc = mur_channel::ChannelService::open(mur_home)?;` and the loaded `fleet`). If `fleet` is a `Fleet` value rather than a reference at that point, pass `&fleet`.

- [x] **1.5 Watch it pass** — same filter as 1.2, plus the neighbours: `cargo nextest run -p mur-core --lib -E 'test(/loop_run::/)'`. Expected: all pass, including `progress_file_written_with_outcome_on_guard_stop` (the progress write is unchanged).

- [x] **1.6 fmt + clippy** (`cargo clippy -p mur-core --all-targets -- -D warnings`), then **commit**:

```
feat(fleet): the loop writes its stop reason and remedy to the channel

`LoopStop` reached only progress.json, so every surface said "finished" for
a run that had hit a cap — the user spent an evening guessing why nothing
continued. One System state-change at the end of `run_guarded` now carries
`stop_reason` and `remedy`, signed as the writer like every other event.
Guard trips map to `failed` (the goal was not reached), kills to
`canceled`, an unanswered approval to `input-required`.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 2 — the fleet rail folds and renders the stop

**Interfaces.**
- Consumes: the Task 1 event shape (`System` + `StateChange` + `stop_reason` + `remedy` payload keys).
- Produces: `pub struct StopNotice { pub reason: String, pub remedy: Option<String> }`; `pub fn fold_stop(events: &[ChannelEvent]) -> Option<StopNotice>`; `RailView.stop: Option<StopNotice>`; `RailView::summary()` emits `  ■ stopped: <reason> — <remedy>` (or without ` — <remedy>` when none) as the second line when `reason != "converged"`.

### Steps

- [ ] **2.1 Write the failing tests** — append to `mur-core/src/cmd/agent/cli/fleet_rail/tests.rs`:

```rust
#[test]
fn fold_stop_reads_the_last_system_stop_event() {
    let evs = vec![
        ev(1, agent("qa"), EventKind::StateChange, json!({"to": "working"})),
        ev(
            2,
            ChannelActor::System,
            EventKind::StateChange,
            json!({"from": "working", "to": "failed",
                   "stop_reason": "max-iterations",
                   "remedy": "raise it: mur fleet settings dev --max-iterations <N>"}),
        ),
    ];
    let s = fold_stop(&evs).expect("a stop notice");
    assert_eq!(s.reason, "max-iterations");
    assert_eq!(s.remedy.as_deref(), Some("raise it: mur fleet settings dev --max-iterations <N>"));

    // A member's state-change is not a stop; a System one without the key is
    // the DAG's ordinary transition, also not a stop.
    let plain = vec![
        ev(1, ChannelActor::System, EventKind::StateChange, json!({"from": "working", "to": "completed"})),
    ];
    assert!(fold_stop(&plain).is_none());
    assert!(fold_stop(&[]).is_none());
}

#[test]
fn summary_names_the_stop_under_the_headline_except_when_converged() {
    let mut view = RailView {
        jobs_line: "fleet · dev   job 2/2".into(),
        members: vec![],
        notice: None,
        stop: Some(StopNotice {
            reason: "deadline".into(),
            remedy: Some("raise it: mur fleet settings dev --deadline <2h>".into()),
        }),
    };
    let lines = view.summary();
    assert_eq!(lines[0], "fleet · dev   job 2/2");
    assert_eq!(lines[1], "  ■ stopped: deadline — raise it: mur fleet settings dev --deadline <2h>");

    view.stop = Some(StopNotice { reason: "stuck".into(), remedy: None });
    assert_eq!(view.summary()[1], "  ■ stopped: stuck");

    view.stop = Some(StopNotice { reason: "converged".into(), remedy: None });
    assert_eq!(view.summary().len(), 1, "converged is not a stop to announce");

    view.stop = None;
    assert_eq!(view.summary().len(), 1);
}
```

  `agent(..)` and `ev(..)` already exist in this file. If `StopNotice` / `fold_stop` are not brought in by the file's existing `use super::*;`, add `use super::{fold_stop, StopNotice};` at the top.

- [ ] **2.2 Watch it fail** — `cargo nextest run -p mur-core --lib -E 'test(/fold_stop_reads|summary_names_the_stop/)'`. Expected: compile error `cannot find function fold_stop` / `no field stop`.

- [ ] **2.3 Add the type and the fold** — in `fleet_rail.rs`, directly below `pub fn fold_members`'s closing brace:

```rust
/// Why the last run stopped and the way out, from the System `state-change`
/// the fleet loop writes at its end (`loop_run::emit_stop_event`).
#[derive(Debug, Clone, PartialEq)]
pub struct StopNotice {
    /// The `outcome_label` word: `max-iterations`, `deadline`, `stuck`, …
    pub reason: String,
    pub remedy: Option<String>,
}

/// The most recent stop event, if any. Only System-authored state-changes
/// that carry `stop_reason` count: a member's own state-change is progress,
/// and the DAG's plain `completed` transition is a step ending, not the run.
pub fn fold_stop(events: &[ChannelEvent]) -> Option<StopNotice> {
    events.iter().rev().find_map(|ev| {
        if ev.kind != EventKind::StateChange || ev.actor != ChannelActor::System {
            return None;
        }
        let reason = field(ev, &["stop_reason"])?.to_string();
        Some(StopNotice {
            reason,
            remedy: field(ev, &["remedy"]).map(str::to_string),
        })
    })
}
```

- [ ] **2.4 Carry it in the view and render it** — add the field to `RailView`:

```rust
    /// The last run's stop, rendered under the headline. `None` while a run is
    /// in flight or before the first run.
    pub stop: Option<StopNotice>,
```

  In `RailView::summary`, replace `let mut out = vec![head];` with:

```rust
        let mut out = vec![head];
        // "finished" is reserved for a converged run; every other stop is
        // announced with its reason and remedy, because the headline alone
        // reads as "done" for a run that hit a cap.
        if let Some(s) = &self.stop
            && s.reason != "converged"
        {
            out.push(match &s.remedy {
                Some(r) => format!("  ■ stopped: {} — {r}", s.reason),
                None => format!("  ■ stopped: {}", s.reason),
            });
        }
```

  In `FleetRail::poll`, where `let view = RailView { jobs_line: …, members, notice: … }` is built, add `stop: fold_stop(&events),` after `members,`. `events` is the verified vector already in scope.

- [ ] **2.5 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/fleet_rail/)'`. Expected: all pass. If a test elsewhere constructs `RailView { .. }` literally and now fails to compile, add `stop: None,` to that literal (search: `command grep -rn "RailView {" mur-core/src`).

- [ ] **2.6 fmt + clippy**, then **commit**:

```
feat(murmur): the fleet rail shows why the last run stopped

Folds the System stop event the loop now writes into `RailView.stop` and
renders it under the headline as `■ stopped: <reason> — <remedy>`.
"finished" stays reserved for a converged run.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 3 — murmur's headline says `stopped: <reason>`, not `finished`

**Interfaces.**
- Consumes: `RailView.stop` (Task 2).
- Produces: `App::finish_auto_fleet` headline is `⛴ fleet <name> finished (took)` only when the run succeeded **and** the rail reports no stop or `converged`; `⛴ fleet <name> stopped: <reason> (took)` when the rail reports any other stop; `⛴ fleet <name> failed (took)` when the step itself failed and there is no stop notice.

### Steps

- [ ] **3.1 Write the failing test** — in `mur-core/src/cmd/agent/cli/app.rs`, right after `an_auto_armed_fleet_rail_lands_in_the_transcript_and_retires`:

```rust
    /// A run that hit a cap is not "finished". The headline takes the rail's
    /// stop word so the transcript's first line already says why.
    #[test]
    fn the_headline_says_stopped_when_the_rail_reports_a_stop() {
        let mut a = app();
        a.arm_auto_fleet("s1", "dev", std::time::Instant::now());
        // Plant a stop event in the fleet's channel so the rail's final poll
        // folds it. The fixture home has no channel yet; create it.
        let svc = mur_channel::ChannelService::open(&a.home).unwrap();
        svc.create_for_fleet("dev", "mur", &["qa".to_string()]).unwrap();
        svc.append(
            "fleet-dev",
            mur_common::channel::ChannelActor::System,
            mur_common::channel::EventKind::StateChange,
            serde_json::json!({"from": "working", "to": "failed",
                               "stop_reason": "max-iterations",
                               "remedy": "raise it: mur fleet settings dev --max-iterations <N>"}),
            None,
        )
        .unwrap();

        a.finish_auto_fleet("s1", true, 4000);

        let last = a.messages.last().expect("outcome message").text.clone();
        assert!(last.starts_with("⛴ fleet dev stopped: max-iterations ("), "got: {last}");
        assert!(!last.lines().next().unwrap().contains("finished"), "got: {last}");
        assert!(last.contains("■ stopped: max-iterations — raise it"), "rail line missing: {last}");
    }
```

  If `App::test_fixture` / `app()` gives an `App` whose `home` is not a writable tempdir, read the fixture (`grep -n "fn test_fixture" -A20 app.rs`) and use the same tempdir it holds; do not create a second home.

- [ ] **3.2 Watch it fail** — `cargo nextest run -p mur-core --lib -E 'test(the_headline_says_stopped)'`. Expected: assertion failure `got: ⛴ fleet dev finished (4s)…`.

- [ ] **3.3 Use the rail's stop in the headline** — in `finish_auto_fleet`, the block that builds `head` becomes:

```rust
        let mut fleet = String::new();
        let mut summary = Vec::new();
        let mut retire = false;
        let mut stop_word: Option<String> = None;
        if let Some(rail) = self.fleet.as_mut() {
            rail.set_run_in_flight(false);
            // A view up to POLL_INTERVAL stale would freeze the wrong states
            // into history. `set_run_in_flight` already busted the poll gate.
            rail.poll(&home, std::time::Instant::now());
            fleet = rail.fleet().to_string();
            summary = rail.view().summary();
            retire = rail.is_auto();
            stop_word = rail
                .view()
                .stop
                .as_ref()
                .filter(|s| s.reason != "converged")
                .map(|s| s.reason.clone());
        }
        if retire {
            self.fleet = None;
        }
        let took = super::follow::fmt_elapsed(chrono::Duration::milliseconds(duration_ms as i64));
        // "finished" is reserved for a run that converged. A cap, a kill or an
        // unanswered approval is a stop, and the first line says so.
        let verdict = match (&stop_word, ok) {
            (Some(reason), _) => format!("stopped: {reason}"),
            (None, true) => "finished".to_string(),
            (None, false) => "failed".to_string(),
        };
        let head = format!("⛴ fleet {fleet} {verdict} ({took})");
```

- [ ] **3.4 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/finish_auto_fleet|auto_armed_fleet|headline_says_stopped/)'`. Expected: the new test and `an_auto_armed_fleet_rail_lands_in_the_transcript_and_retires` both pass (that one has no stop event, so it still reads `finished`).

- [ ] **3.5 fmt + clippy**, then **commit**:

```
fix(murmur): a capped fleet run is "stopped: <reason>", not "finished"

The headline took its word from the step's ok flag alone, so a run that
hit max-iterations read as finished. It now takes the rail's stop when
there is one; "finished" is reserved for a converged run.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 4 — the settlement card's stop row carries a remedy

**Interfaces.**
- Consumes: `StopKind` (exists: `EndTurn`, `MaxIterations`, `TokenBudget`, `LoopDetected`, `MaxTokens`), `render(&TurnLedger)` (exists, emits `  ⚠ stopped at {kind} ({n} iterations) — output may be incomplete`).
- Produces: `impl StopKind { pub fn remedy(self) -> Option<&'static str> }`; the row becomes `  ⚠ stopped at {kind} ({n} iterations) — output may be incomplete · {remedy}`.

### Steps

- [ ] **4.1 Write the failing test** — in `mur-agent-runtime/src/turn_ledger.rs`'s test module, next to the existing test that asserts `"output may be incomplete"`:

```rust
    /// Every unclean stop says what to do about it, next to the fact. Naming
    /// the knob is the difference between "the agent gave up" and "raise
    /// hitl.max_tokens".
    #[test]
    fn every_unclean_stop_names_its_remedy() {
        assert_eq!(StopKind::EndTurn.remedy(), None);
        for k in [StopKind::MaxIterations, StopKind::TokenBudget, StopKind::LoopDetected, StopKind::MaxTokens] {
            assert!(k.remedy().is_some(), "{k:?}");
        }
        assert!(StopKind::MaxIterations.remedy().unwrap().contains("hitl.max_iterations"));
        assert!(StopKind::TokenBudget.remedy().unwrap().contains("hitl.max_tokens"));

        let mut ledger = TurnLedger::default();
        ledger.stop = StopKind::TokenBudget;
        ledger.iterations = 17;
        let card = render(&ledger);
        assert!(
            card.contains("⚠ stopped at token budget (17 iterations) — output may be incomplete · raise hitl.max_tokens"),
            "{card}"
        );
    }
```

  If `TurnLedger` has no `stop` field but a setter, use the setter the existing "output may be incomplete" test uses; mirror that test's construction exactly.

- [ ] **4.2 Watch it fail** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(every_unclean_stop_names_its_remedy)'`. Expected: compile error `no method named remedy`.

- [ ] **4.3 Add the remedy and render it** — in `impl StopKind`, after `as_str`:

```rust
    /// What to do about it, in today's knobs. `hitl.max_iterations` and
    /// `hitl.max_tokens` live in the agent's profile.yaml until the `limits:`
    /// schema replaces them; the settlement card is where the user learns
    /// which one bit, so it names it.
    pub fn remedy(self) -> Option<&'static str> {
        match self {
            StopKind::EndTurn => None,
            StopKind::MaxIterations => Some("raise hitl.max_iterations in the agent's profile.yaml and restart it"),
            StopKind::TokenBudget => Some("raise hitl.max_tokens in the agent's profile.yaml and restart it"),
            StopKind::LoopDetected => Some("the last tool call repeated with identical arguments — change the ask, or the tool's input"),
            StopKind::MaxTokens => Some("the model's output limit — ask it to continue from where it stopped"),
        }
    }
```

  In `render`, replace the `if !ledger.stop.is_clean() { … }` block with:

```rust
    if !ledger.stop.is_clean() {
        out.push_str(&format!(
            "  ⚠ stopped at {} ({} iterations) — output may be incomplete",
            ledger.stop.as_str(),
            ledger.iterations
        ));
        if let Some(r) = ledger.stop.remedy() {
            out.push_str(&format!(" · {r}"));
        }
        out.push('\n');
    }
```

- [ ] **4.4 Watch it pass** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(/turn_ledger/)'`. Expected: all pass. The murmur card parser (`mur-core/src/cmd/agent/cli/settlement.rs`) treats the line as a `⚠` row and wraps it; no change there.

- [ ] **4.5 fmt + clippy** (`cargo clippy -p mur-agent-runtime --all-targets -- -D warnings`), then **commit**:

```
feat(runtime): the settlement card's stop row names the knob that bit

"stopped at token budget (17 iterations)" told the user the agent gave up;
it did not say that hitl.max_tokens is the ceiling or where it lives. Each
unclean StopKind now carries a one-line remedy, rendered on the same row.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## After the last task

- One PR for the branch: `feat: stop reasons reach the settlement card and the fleet rail (spec §3.7, step 1)`. Body lists the four commits and the manual check below.
- Manual check after `build.sh --install`: run `mur fleet run <fleet> --loop --max-iterations 1` on a fleet whose members are up; murmur (with `--fleet <fleet>`) shows `⛴ fleet <fleet> stopped: max-iterations (…)` and the `■ stopped: max-iterations — raise it: …` line; `~/.mur/channels/fleet-<fleet>/events.jsonl` ends with a System `state-change` carrying `stop_reason`.
- Docs: the `update-docs` skill — the agent-cli page's fleet-progress section gains one sentence on the `■ stopped:` line. No README change (behaviour, not surface).
