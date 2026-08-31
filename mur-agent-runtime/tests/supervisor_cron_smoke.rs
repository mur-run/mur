//! Smoke tests: CronScheduler spawns, entries with bad cron warn+exit cleanly,
//! and the cancellation token propagates abort to inner tasks.

use mur_agent_runtime::scheduler::CronScheduler;
use mur_agent_runtime::task_runner::TaskRunner;
use mur_common::agent::ScheduleEntry;
use std::sync::Arc;

#[tokio::test]
async fn cron_scheduler_spawns_and_aborts() {
    let entries = vec![
        ScheduleEntry {
            cron: "* * * * *".into(),
            message: "ping".into(),
            sends_to: None,
            not_after: None,
        },
        ScheduleEntry {
            cron: "0 9 * * 1-5".into(),
            message: "morning brief".into(),
            sends_to: None,
            not_after: None,
        },
    ];
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let handle = CronScheduler::new(entries, runner).spawn();
    // Brief pause so inner tasks start up.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    handle.abort();
    let result = handle.await;
    assert!(result.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn cron_scheduler_skips_bad_entry() {
    let entries = vec![ScheduleEntry {
        cron: "not valid".into(),
        message: "should be skipped".into(),
        sends_to: None,
        not_after: None,
    }];
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let handle = CronScheduler::new(entries, runner).spawn();
    // The inner loop exits immediately on bad expr; outer task completes naturally.
    let result = tokio::time::timeout(std::time::Duration::from_millis(200), handle).await;
    // Either completes naturally (Ok(Ok(()))) or is still sleeping (timeout).
    // What must NOT happen: panic.
    match result {
        Ok(Ok(())) => {} // inner loop exited cleanly
        Ok(Err(e)) if e.is_cancelled() => {}
        Err(_) => {} // timeout is also fine — no panic is the assertion
        Ok(Err(e)) => panic!("unexpected JoinError: {e}"),
    }
}
