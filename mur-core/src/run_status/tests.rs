use super::*;

const STALE_AFTER_SECS: i64 = 30;

fn stale_after() -> chrono::Duration {
    chrono::Duration::seconds(STALE_AFTER_SECS)
}

fn run(state: State, pid: u32, heartbeat_age_secs: Option<i64>, now: DateTime<Utc>) -> RunState {
    RunState {
        schema: RUN_SCHEMA,
        run_id: "r".into(),
        channel_id: None,
        kind: RunKind::Job,
        label: "l".into(),
        pid,
        started_at: now - chrono::Duration::seconds(600),
        last_heartbeat_at: heartbeat_age_secs.map(|s| now - chrono::Duration::seconds(s)),
        state,
        steps: vec![],
        blocked_on: None,
        binary_version: "0.0.0-test".into(),
        build_sha: "deadbee".into(),
    }
}

/// A pid that is certainly not running: spawn a trivial child, wait for it,
/// and reuse its reaped pid. Checking a literal pid would be a guess.
///
/// NEVER call external utilities that may not exist on the target
/// platform: `true` does not ship with Windows (a CI workspace gate
/// without true.exe would panic before testing classification), so the
/// helper is cfg'd — `true` on Unix, the always-present `cmd` on
/// Windows.
fn dead_pid() -> u32 {
    #[cfg(unix)]
    {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let pid = child.id();
        child.wait().expect("reap child");
        pid
    }
    #[cfg(windows)]
    {
        // `cmd /C exit 1` returns immediately and the child is
        // definitely dead by the time its pid is reused. `cmd` is
        // guaranteed to exist on every Windows install.
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit 1"])
            .spawn()
            .expect("spawn cmd");
        let pid = child.id();
        child.wait().expect("reap child");
        pid
    }
}

#[test]
fn every_state_liveness_cell() {
    let now = Utc::now();
    let live = std::process::id();
    let dead = dead_pid();

    // Non-terminal + live process + fresh heartbeat => alive.
    for state in [State::Running, State::Blocked] {
        let s = classify(run(state, live, Some(1), now), now, stale_after());
        assert_eq!(s.state, state);
        assert_eq!(s.liveness, Liveness::Alive, "{state:?} with a fresh beat");
    }

    // Non-terminal + live process + expired heartbeat => stalled.
    for state in [State::Running, State::Blocked] {
        let s = classify(
            run(state, live, Some(STALE_AFTER_SECS + 1), now),
            now,
            stale_after(),
        );
        assert_eq!(s.liveness, Liveness::Stalled, "{state:?} with a dead beat");
    }

    // Non-terminal + no process => dead, whatever the heartbeat said.
    for state in [State::Running, State::Blocked] {
        let s = classify(run(state, dead, Some(1), now), now, stale_after());
        assert_eq!(s.liveness, Liveness::Dead, "{state:?} with no process");
    }

    // Non-terminal + live process + rebuilt (no heartbeat) => unknown.
    let s = classify(run(State::Running, live, None, now), now, stale_after());
    assert_eq!(s.liveness, Liveness::Unknown);

    // Non-terminal + dead process + rebuilt (no heartbeat) => unknown,
    // NOT dead: the absent-heartbeat check precedes the pid check, so a
    // pid whose liveness is platform-dependent (0 on Windows reads
    // dead via a failing OpenProcess) can never pick the verdict.
    let s = classify(run(State::Running, dead, None, now), now, stale_after());
    assert_eq!(
        s.liveness,
        Liveness::Unknown,
        "absent heartbeat must win over a dead pid"
    );

    // Terminal => n/a regardless of process or heartbeat.
    for state in [State::Done, State::Failed, State::Stopped] {
        for pid in [live, dead] {
            for beat in [Some(1), Some(STALE_AFTER_SECS + 1), None] {
                let s = classify(run(state, pid, beat, now), now, stale_after());
                assert_eq!(
                    s.liveness,
                    Liveness::NotApplicable,
                    "{state:?} must not report liveness"
                );
            }
        }
    }
}

/// Negative control for the reported defect. A test that only asserts a
/// live process reports `alive` proves nothing: freezing the heartbeat
/// while the process stays up MUST flip the verdict.
#[test]
fn frozen_heartbeat_flips_running_to_stalled() {
    let now = Utc::now();
    let live = std::process::id();
    let fresh = classify(run(State::Running, live, Some(1), now), now, stale_after());
    let frozen = classify(
        run(State::Running, live, Some(STALE_AFTER_SECS + 1), now),
        now,
        stale_after(),
    );
    assert_eq!(fresh.liveness, Liveness::Alive);
    assert_eq!(frozen.liveness, Liveness::Stalled);
    assert_ne!(
        fresh.liveness, frozen.liveness,
        "heartbeat is not consulted"
    );
}

/// Negative control: a killed orchestrator must never keep reporting
/// `running`/`alive`. `state` stays `running` because nothing wrote a
/// terminal state — that pair IS what a crash looks like.
#[test]
fn killed_orchestrator_reports_dead_not_running() {
    let now = Utc::now();
    let s = classify(
        run(State::Running, dead_pid(), Some(1), now),
        now,
        stale_after(),
    );
    assert_eq!(
        s.state,
        State::Running,
        "no terminal state was ever written"
    );
    assert_eq!(s.liveness, Liveness::Dead);
    assert!(!s.state.is_terminal(), "a crashed run is not finished");
}

#[test]
fn liveness_is_never_persisted() {
    let now = Utc::now();
    let json =
        serde_json::to_string(&run(State::Running, std::process::id(), Some(1), now)).unwrap();
    assert!(
        !json.contains("liveness"),
        "liveness must be derived, never stored: {json}"
    );
}

/// The executor's heartbeat ticker and `status_of`'s stale threshold
/// both read `Config::load_or_default` — a zero interval must reach
/// NEITHER of them. A 0×0 stale threshold would classify a fresh beat
/// as STALLED, so `Alive` here is only reachable through the clamped
/// 10s×3=30s default.
#[test]
fn status_of_clamps_a_zero_heartbeat_interval_to_the_default_threshold() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    std::fs::write(
        mur_home.join("config.yaml"),
        "runs:\n  heartbeat_interval_secs: 0\n  heartbeat_stale_after_intervals: 0\n",
    )
    .unwrap();

    let now = Utc::now();
    let record = run(State::Running, std::process::id(), Some(5), now);
    store::save(mur_home, &record).unwrap();

    let status = status_of(mur_home, &record.run_id)
        .unwrap()
        .expect("run was just saved");
    assert_eq!(
        status.liveness,
        Liveness::Alive,
        "a 5s-old heartbeat must read Alive under the clamped 30s \
             default — Stalled would mean the zero interval leaked into the \
             stale threshold"
    );
}

/// `status_of` must load `<mur_home>/config.yaml`, not just `mur_home`
/// itself — `Config::load_or_default` takes a file path, and silently
/// returns `Config::default()` on any read failure (including "this
/// path is a directory"). A wrong path here does not error; it just
/// makes every `runs:` setting a dead knob a user can change with no
/// observable effect. Prove the config is actually read by giving it a
/// `stale_after` far stricter than the default and checking the
/// classification only that value can produce.
#[test]
fn status_of_reads_the_configured_stale_after_not_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();

    // Default stale_after is 10s * 3 = 30s (RunsConfig's defaults).
    // A 5s-old heartbeat reads `Alive` under that default and `Stalled`
    // under this 1s config — so the two paths cannot agree by accident.
    std::fs::write(
        mur_home.join("config.yaml"),
        "runs:\n  heartbeat_interval_secs: 1\n  heartbeat_stale_after_intervals: 1\n",
    )
    .unwrap();

    let now = Utc::now();
    let record = run(State::Running, std::process::id(), Some(5), now);
    store::save(mur_home, &record).unwrap();

    let status = status_of(mur_home, &record.run_id)
        .unwrap()
        .expect("run was just saved");
    assert_eq!(
        status.liveness,
        Liveness::Stalled,
        "status_of computed Alive, which is only reachable via the \
             default 30s stale_after — config.yaml at mur_home is not \
             being read, so the configured 1s stale_after never took effect"
    );
}

/// `status_of` must not pretend a run never existed when the channel it
/// could rebuild from is genuinely unreadable: with run.json missing and
/// the channel read faulting, the error must surface — not Ok(None).
#[test]
fn status_of_reports_a_genuine_channel_fault_instead_of_none() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let svc = mur_channel::ChannelService::open(mur_home).unwrap();
    let ch = svc.create_for_workflow("faulty-channel").unwrap();
    let dir = store::runs_dir(mur_home).join("run-x");
    std::fs::create_dir_all(&dir).unwrap();
    store::save_sidecar(
        mur_home,
        "run-x",
        &Sidecar {
            schema: SIDECAR_SCHEMA,
            channel_id: ch.id.clone(),
            kind: RunKind::Job,
            first_seq: 0,
        },
    )
    .unwrap();
    // No run.json -> the rebuild path is the only route; sabotage it.
    // Portable sabotage: `events.jsonl` becomes a DIRECTORY, so reading it
    // fails with a non-`NotFound` error on every platform (EISDIR on Unix,
    // ERROR_ACCESS_DENIED on Windows). Replacing the channel DIR with a
    // file does NOT work: Windows maps the resulting path error to
    // `NotFound`, which `load_events` legitimately reads as absence, so
    // the fault this test exists to catch would be swallowed there.
    let chan_dir = mur_home.join("channels").join(&ch.id);
    let events = chan_dir.join("events.jsonl");
    let _ = std::fs::remove_file(&events);
    std::fs::create_dir_all(&events).unwrap();

    let err = status_of(mur_home, "run-x")
        .expect_err("a genuine channel read fault must surface as an error, not Ok(None)");
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&ch.id),
        "the error must name the channel: {msg}"
    );
}

/// A corrupt run.json must not take the run down with it: the
/// sidecar.json index survives the cache, and status_of must fall back to
/// a rebuilt record that honestly reports an unknown heartbeat.
#[test]
fn status_of_rebuilds_from_the_channel_when_the_cache_is_corrupt() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();

    // A channel with a finished (failed) run's worth of events, written
    // the way the executor writes them — run_id stamped on each payload.
    let svc = mur_channel::ChannelService::open(mur_home).unwrap();
    let ch = svc.create_for_workflow("corrupt-cache").unwrap();
    svc.append_delegation(&ch.id, "pm", "child-1", None, Some("run-c"))
        .unwrap();
    svc.transition(
        &ch.id,
        mur_common::channel::ChannelState::Failed,
        mur_common::channel::ChannelActor::System,
        Some("run-c"),
    )
    .unwrap();

    // The run directory with the sidecar but a GARBLED run.json.
    let dir = store::runs_dir(mur_home).join("run-c");
    std::fs::create_dir_all(&dir).unwrap();
    store::save_sidecar(
        mur_home,
        "run-c",
        &Sidecar {
            schema: SIDECAR_SCHEMA,
            channel_id: ch.id.clone(),
            kind: RunKind::Workflow,
            first_seq: 0,
        },
    )
    .unwrap();
    std::fs::write(dir.join("run.json"), b"{ this is not json").unwrap();

    let status = status_of(mur_home, "run-c")
        .unwrap()
        .expect("a corrupt cache must fall back to the channel, not return None");
    assert_eq!(
        status.state,
        State::Failed,
        "rebuilt state must come from the channel"
    );
    assert_eq!(
        status.liveness,
        Liveness::NotApplicable,
        "a finished rebuilt run reports no liveness"
    );
    assert!(
        status.run.last_heartbeat_at.is_none(),
        "rebuilt heartbeat must be unknown"
    );
}

/// THE regression for the review's parseable-cache finding: the channel's
/// Completed transition succeeded but the terminal run.json write failed,
/// so the cache still says `running` with a fresh heartbeat and a live
/// pid. `status_of` must report the channel's `done` — not `running` +
/// whatever the pid says — and the heartbeat it reports must be the
/// cache's real one, not a fabricated value.
#[test]
fn status_of_reconciles_a_parseable_running_cache_with_the_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();

    // The channel's authoritative tail: this run completed.
    let svc = mur_channel::ChannelService::open(mur_home).unwrap();
    let ch = svc.create_for_workflow("reconcile").unwrap();
    svc.append_delegation(&ch.id, "pm", "child-1", None, Some("run-r"))
        .unwrap();
    svc.transition(
        &ch.id,
        mur_common::channel::ChannelState::Completed,
        mur_common::channel::ChannelActor::System,
        Some("run-r"),
    )
    .unwrap();

    // The cache: still running, with a real (fresh) heartbeat and live
    // pid — the exact shape of "channel Completed succeeded, terminal
    // run.json write failed".
    let now = Utc::now();
    let mut record = run(State::Running, std::process::id(), Some(1), now);
    record.run_id = "run-r".into();
    record.channel_id = Some(ch.id.clone());
    let cached_heartbeat = record.last_heartbeat_at;
    store::save(mur_home, &record).unwrap();
    store::save_sidecar(
        mur_home,
        "run-r",
        &Sidecar {
            schema: SIDECAR_SCHEMA,
            channel_id: ch.id.clone(),
            kind: RunKind::Workflow,
            first_seq: 0,
        },
    )
    .unwrap();

    let status = status_of(mur_home, "run-r")
        .unwrap()
        .expect("the run was just recorded");
    assert_eq!(
        status.state,
        State::Done,
        "the channel wins over a parseable cache that still says running"
    );
    assert_eq!(
        status.liveness,
        Liveness::NotApplicable,
        "a finished run reports no liveness"
    );
    assert_eq!(
        status.run.last_heartbeat_at, cached_heartbeat,
        "the cache's real heartbeat is retained, never fabricated"
    );
}

/// A corrupt sidecar.json must not silently disable reconciliation: the
/// operator is told why (a warn on the read — the write path already
/// warns, the read must too), and status_of still succeeds with the
/// cache's answer.
#[test]
fn status_of_warns_when_the_reconcile_sidecar_read_fails() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    // A writer that captures log lines so the warn is asserted, not
    // assumed.
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();

    // The channel's authoritative tail: this run completed.
    let svc = mur_channel::ChannelService::open(mur_home).unwrap();
    let ch = svc.create_for_workflow("corrupt-sidecar").unwrap();
    svc.append_delegation(&ch.id, "pm", "child-1", None, Some("run-s"))
        .unwrap();
    svc.transition(
        &ch.id,
        mur_common::channel::ChannelState::Completed,
        mur_common::channel::ChannelActor::System,
        Some("run-s"),
    )
    .unwrap();

    // A parseable, still-running cache — reconciliation would override it
    // to done, but the sidecar is corrupt, so the channel cannot be
    // consulted. The cache answer stands, and the operator is told why.
    let now = Utc::now();
    let mut record = run(State::Running, std::process::id(), Some(1), now);
    record.run_id = "run-s".into();
    store::save(mur_home, &record).unwrap();
    let dir = store::runs_dir(mur_home).join("run-s");
    std::fs::write(dir.join("sidecar.json"), b"{ not json").unwrap();
    assert!(
        store::load_sidecar(mur_home, "run-s").is_err(),
        "precondition: the sidecar really is corrupt"
    );

    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::WARN)
        .finish();
    let status = tracing::subscriber::with_default(subscriber, || status_of(mur_home, "run-s"))
        .unwrap()
        .expect("a corrupt sidecar must not fail the status itself");
    assert_eq!(
        status.state,
        State::Running,
        "without a readable sidecar the cache's answer stands"
    );

    let logged = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("sidecar"),
        "the corrupt sidecar read must be warned, not silently skipped: {logged}"
    );
}

/// An unreadable sidecar on the rebuild path is an I/O fault, not an
/// absent run. `load_sidecar` already separates the two; `rebuild_for`
/// must not put them back together, or a permission error reads to the
/// operator as "no such run".
#[test]
fn status_of_reports_an_unreadable_sidecar_instead_of_no_such_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();

    // No `run.json` at all: the cache misses and `status_of` falls to the
    // rebuild path, where the only index is this corrupt sidecar.
    let dir = store::runs_dir(mur_home).join("run-u");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("sidecar.json"), b"{ not json").unwrap();

    let error = status_of(mur_home, "run-u")
        .expect_err("an unreadable sidecar must not be reported as 'no such run'");
    assert!(
        format!("{error:#}").contains("sidecar"),
        "the fault must name the sidecar it could not read: {error:#}"
    );
}

/// Reconciliation must be bounded to the run: another run's terminal
/// state on the same channel (different run_id) must not override this
/// run's still-running cache.
#[test]
fn status_of_reconciliation_ignores_other_runs_on_the_same_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();

    let svc = mur_channel::ChannelService::open(mur_home).unwrap();
    let ch = svc.create_for_workflow("reconcile-shared").unwrap();
    // Run B completed on the shared channel; run R is still running.
    svc.append_delegation(&ch.id, "pm", "child-b", None, Some("run-b"))
        .unwrap();
    svc.transition(
        &ch.id,
        mur_common::channel::ChannelState::Completed,
        mur_common::channel::ChannelActor::System,
        Some("run-b"),
    )
    .unwrap();

    let now = Utc::now();
    let mut record = run(State::Running, std::process::id(), Some(1), now);
    record.run_id = "run-r".into();
    store::save(mur_home, &record).unwrap();
    store::save_sidecar(
        mur_home,
        "run-r",
        &Sidecar {
            schema: SIDECAR_SCHEMA,
            channel_id: ch.id.clone(),
            kind: RunKind::Workflow,
            first_seq: 0,
        },
    )
    .unwrap();

    let status = status_of(mur_home, "run-r").unwrap().expect("recorded run");
    assert_eq!(
        status.state,
        State::Running,
        "B's completion on the same channel must not end R"
    );
    assert_eq!(
        status.liveness,
        Liveness::Alive,
        "R is live and healthy; B's state must not leak into its liveness"
    );
}

#[test]
fn run_ids_are_one_safe_path_segment() {
    assert!(valid_run_id("fleet-deep-research-0199"));
    assert!(!valid_run_id(""));
    assert!(!valid_run_id("../x"));
    assert!(!valid_run_id("a b"));
}
