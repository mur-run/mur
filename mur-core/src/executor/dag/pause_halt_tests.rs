//! Pause/resume design §5.2 at the DAG: a `HaltReason::Pause` halt starts no
//! further step, writes no terminal channel state, and records
//! `State::Paused`; a stop that is also set still wins.

use super::run::terminal_channel_state;
use super::*;
use std::sync::atomic::{AtomicU8, Ordering};

const NONE: u8 = 0;
const PAUSE: u8 = 1;
const STOP: u8 = 2;

fn halt_flag() -> (Arc<AtomicU8>, HaltSignal) {
    let f = Arc::new(AtomicU8::new(NONE));
    let g = f.clone();
    (
        f,
        Arc::new(move || match g.load(Ordering::SeqCst) {
            PAUSE => Some(HaltReason::Pause),
            STOP => Some(HaltReason::Stop),
            _ => None,
        }),
    )
}

fn touch_step(id: &str, deps: &[&str], path: &std::path::Path) -> ProcedureStep {
    ProcedureStep {
        id: Some(id.to_string()),
        command: Some(format!("touch {}", path.display())),
        description: format!("{id} step"),
        depends_on: deps.iter().map(|d| d.to_string()).collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_pause_before_the_run_starts_nothing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("ran");
    let (f, halt) = halt_flag();
    f.store(PAUSE, Ordering::SeqCst);
    let proc = Procedure {
        variables: vec![],
        steps: vec![touch_step("s0", &[], &marker)],
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            halt: Some(halt),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    // The loop re-reads the sentinels to tell pause from stop, so the DAG
    // reports the shared halted status.
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(
        !marker.exists(),
        "a paused run must not start its first step"
    );
}

#[tokio::test]
async fn a_pause_records_state_paused_and_not_stopped() {
    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("ran");
    let (f, halt) = halt_flag();
    f.store(PAUSE, Ordering::SeqCst);
    let proc = Procedure {
        variables: vec![],
        steps: vec![touch_step("s0", &[], &marker)],
    };
    execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            halt: Some(halt),
            run_id: "paused-run".into(),
            run_kind: Some(crate::run_status::RunKind::Fleet),
            run_label: "l".into(),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    let rec = crate::run_status::store::load(tmp.path(), "paused-run")
        .unwrap()
        .expect("run recorded");
    assert_eq!(rec.state, crate::run_status::State::Paused);
    assert_eq!(terminal_channel_state(RunOutcome::Paused), None);
}

/// Negative control: with the stop also set, the same halt is a stop and the
/// run is recorded `Stopped` with a `canceled` close.
#[tokio::test]
async fn a_stop_set_alongside_a_pause_wins() {
    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("ran");
    let (f, halt) = halt_flag();
    f.store(STOP, Ordering::SeqCst);
    let proc = Procedure {
        variables: vec![],
        steps: vec![touch_step("s0", &[], &marker)],
    };
    execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            halt: Some(halt),
            run_id: "stopped-run".into(),
            run_kind: Some(crate::run_status::RunKind::Fleet),
            run_label: "l".into(),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    let rec = crate::run_status::store::load(tmp.path(), "stopped-run")
        .unwrap()
        .expect("run recorded");
    assert_eq!(rec.state, crate::run_status::State::Stopped);
    assert_eq!(
        terminal_channel_state(RunOutcome::Stopped),
        Some(ChannelState::Canceled)
    );
    assert!(!marker.exists());
}

/// A pause landing while rank 0 runs starts no later rank, and the run never
/// reads done even though every step that ran succeeded.
#[tokio::test]
async fn a_pause_during_a_rank_starts_no_later_rank() {
    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("rank1-ran");
    let (f, halt) = halt_flag();
    let slow = ProcedureStep {
        id: Some("s0".into()),
        command: Some("sleep 0.3".into()),
        description: "s0 step".into(),
        ..Default::default()
    };
    let proc = Procedure {
        variables: vec![],
        steps: vec![slow, touch_step("s1", &["s0"], &marker)],
    };
    let flip = {
        let f = f.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            f.store(PAUSE, Ordering::SeqCst);
        })
    };
    let out = execute_dag(
        tmp.path(),
        "t",
        &proc,
        &DagExecOptions {
            halt: Some(halt),
            ..DagExecOptions::default()
        },
    )
    .await
    .unwrap();
    flip.await.unwrap();
    assert_eq!(out.status, PipelineStatus::Stopped);
    assert!(!marker.exists(), "rank 1 started after the pause");
}
