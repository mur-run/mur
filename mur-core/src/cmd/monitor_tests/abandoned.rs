//! #1622: a `mur_run` monitor whose run's process died without recording a
//! result settles as `abandoned` past the grace window — through the real
//! adapter and scheduler — and `show` explains it instead of leaving the
//! user to decode `completed / abandoned`.

use super::*;
use crate::monitor::adapters::mur_run::abandon_grace;

/// A pid that existed and is now reaped — `pid_alive` reads it as dead.
fn dead_pid() -> u32 {
    #[cfg(unix)]
    let mut child = std::process::Command::new("true").spawn().unwrap();
    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "exit 0"])
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

/// A monitor on `run-1`, then the run rewritten as a crash: still `running`,
/// its process gone, its last heartbeat `beat_age` before the tick.
fn crashed_run_monitor(
    beat_age: chrono::Duration,
) -> (
    tempfile::TempDir,
    mur_common::test_env::EnvGuard,
    String,
    DateTime<Utc>,
) {
    let (d, g) = home();
    go(
        d.path(),
        MonitorAction::Add {
            file: spec_file(d.path(), "mur_run", "run-1"),
            started_at: None,
        },
    )
    .unwrap();
    let id = MonitorStore::open(d.path())
        .unwrap()
        .list(&ListFilter::default())
        .unwrap()[0]
        .id
        .clone();
    let now = Utc::now();
    let mut run = run_store::load(d.path(), "run-1").unwrap().unwrap();
    run.pid = dead_pid();
    run.last_heartbeat_at = Some(now - beat_age);
    run_store::save(d.path(), &run).unwrap();
    (d, g, id, now)
}

fn tick_once(d: &Path, now: DateTime<Utc>) {
    let s = MonitorStore::open(d).unwrap();
    // `add` ran at `t0()`, so the monitor is long due by the wall clock the
    // adapter itself reads.
    let rep = mur_monitor::scheduler::tick(&s, &crate::monitor::registry(d), now, "t", 8).unwrap();
    assert_eq!(rep.claimed, 1, "the monitor must be observed this tick");
}

#[test]
fn a_crashed_run_past_grace_settles_as_abandoned_and_show_explains_it() {
    let grace = abandon_grace(&mur_common::config::RunsConfig::default());
    let (d, _g, id, now) = crashed_run_monitor(grace + chrono::Duration::minutes(1));
    tick_once(d.path(), now);

    let s = MonitorStore::open(d.path()).unwrap();
    let r = s.get(&id).unwrap().unwrap();
    assert_eq!(
        (r.state, r.outcome),
        (
            MonitorState::Completed,
            mur_monitor::state::Outcome::Abandoned
        )
    );
    // The murmur footer counts exactly this list (default filter), so an
    // empty one clears both `MONITOR (N)` and the issue count.
    assert!(
        s.list(&ListFilter::default()).unwrap().is_empty(),
        "the footer's MONITOR (N) no longer counts it"
    );

    let out = go(
        d.path(),
        MonitorAction::Show {
            id: id.clone(),
            history: false,
        },
    )
    .unwrap();
    assert!(out.contains("completed / abandoned"), "{out}");
    let note = out
        .lines()
        .find(|l| l.contains("abandoned:"))
        .unwrap_or_else(|| panic!("no abandoned explanation in:\n{out}"));
    assert!(note.contains("not a failure"), "{note}");
}

#[test]
fn a_crashed_run_within_grace_keeps_watching_as_unknown() {
    let (d, _g, id, now) = crashed_run_monitor(chrono::Duration::seconds(5));
    tick_once(d.path(), now);
    let r = MonitorStore::open(d.path())
        .unwrap()
        .get(&id)
        .unwrap()
        .unwrap();
    assert_eq!(r.outcome, mur_monitor::state::Outcome::Unknown);
    assert_ne!(r.state, MonitorState::Completed);
}
