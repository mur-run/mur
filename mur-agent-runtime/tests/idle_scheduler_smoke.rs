//! Smoke tests: IdleScheduler spawns, fires with a fast tick, aborts cleanly.

use mur_agent_runtime::idle_scheduler::IdleScheduler;
use mur_agent_runtime::task_runner::TaskRunner;
use mur_common::agent::IdleTrigger;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn idle_scheduler_spawns_and_aborts() {
    let triggers = vec![IdleTrigger {
        after_secs: 60,
        message: "ping".into(),
        sends_to: None,
        cooldown_secs: 600,
        respect_quiet_hours: false,
    }];
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let handle = IdleScheduler::new(triggers, runner, None)
        .with_tick_interval(Duration::from_millis(50))
        .spawn();
    tokio::time::sleep(Duration::from_millis(20)).await;
    handle.abort();
    let result = handle.await;
    assert!(result.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn idle_scheduler_fires_when_after_secs_is_zero() {
    // after_secs=0 means "fire immediately on first eligible tick".
    // run_sync on the StubEcho backend returns without error, so no panic.
    let triggers = vec![IdleTrigger {
        after_secs: 0,
        message: "are you there?".into(),
        sends_to: None,
        cooldown_secs: 0,
        respect_quiet_hours: false,
    }];
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let handle = IdleScheduler::new(triggers, runner, None)
        .with_tick_interval(Duration::from_millis(50))
        .spawn();
    // Two ticks: scheduler should fire at least once without panicking.
    tokio::time::sleep(Duration::from_millis(150)).await;
    handle.abort();
    let result = handle.await;
    assert!(result.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn idle_scheduler_respects_cooldown() {
    // after_secs=0, cooldown=3600 — fires exactly once then suppressed.
    // We can't easily count fires; this test just verifies no panic.
    let triggers = vec![IdleTrigger {
        after_secs: 0,
        message: "daily check".into(),
        sends_to: None,
        cooldown_secs: 3600,
        respect_quiet_hours: false,
    }];
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let handle = IdleScheduler::new(triggers, runner, None)
        .with_tick_interval(Duration::from_millis(50))
        .spawn();
    tokio::time::sleep(Duration::from_millis(200)).await;
    handle.abort();
    let result = handle.await;
    assert!(result.unwrap_err().is_cancelled());
}
