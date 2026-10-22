//! The display-only step observer the fleet loop hands the DAG executor:
//! it mutates the shared progress record and prints one log line per step
//! event. Best-effort throughout: the closure never panics (poison-safe lock)
//! and never affects execution. Moved out of `guarded.rs` unchanged to keep
//! that file under the 800-line limit.

use super::*;
use crate::executor::dag::{StepEvent, StepEventKind};

pub(super) fn step_observer(
    progress: Arc<Mutex<RunProgress>>,
    step_home: std::path::PathBuf,
    step_fleet: String,
    price_per_1k: f64,
) -> Arc<dyn Fn(StepEvent) + Send + Sync> {
    let step_progress = progress;
    Arc::new(move |e: StepEvent| {
        let mut g = step_progress.lock().unwrap_or_else(|x| x.into_inner());
        let Some(sp) = g.steps.iter_mut().find(|s| s.id == e.id) else {
            return;
        };
        let now = chrono::Utc::now().to_rfc3339();
        match e.kind {
            StepEventKind::Started => {
                sp.state = StepState::Running;
                sp.started_at = Some(now);
            }
            StepEventKind::Blocked => {
                // Waiting on a human. Not Running (nothing is executing)
                // and not Failed (nothing went wrong): park it back at
                // Pending, which is what it is — work still to do — and
                // say so on its own line so the rail is not silent about
                // why the fleet stopped short.
                sp.state = StepState::Pending;
                println!("⏸ {} awaiting approval", sp.id);
            }
            // Halted by the kill-switch: back to Pending (work not done,
            // nothing failed), with its own line like a blocked step. The
            // halt reason travels in `error`, so a pause is not printed as a
            // stop.
            StepEventKind::Stopped => {
                sp.state = StepState::Pending;
                sp.ended_at = Some(now);
                let paused = e.error.as_deref() == Some(crate::executor::dag::PAUSED_REASON);
                println!(
                    "{} {} {}",
                    if paused { "⏸" } else { "⏹" },
                    sp.id,
                    if paused { "paused" } else { "stopped" }
                );
            }
            // Skipped only arises without a channel and a fleet run always
            // has one; should it ever arrive, it did not fail.
            StepEventKind::Done | StepEventKind::Failed | StepEventKind::Skipped => {
                let done = e.kind != StepEventKind::Failed;
                sp.state = if done {
                    StepState::Done
                } else {
                    StepState::Failed
                };
                if e.tokens_used > 0 {
                    sp.cost_usd = Some(iteration_cost_usd(e.tokens_used, price_per_1k));
                }
                // `✓ s2 research dr_worker_2 $0.08 42s`
                let mark = if done { '✓' } else { '✗' };
                let phase = sp.phase.label();
                let worker = sp.worker.clone().unwrap_or_default();
                let cost = sp.cost_usd.map(|c| format!(" ${c:.2}")).unwrap_or_default();
                let elapsed = sp
                    .started_at
                    .as_deref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .and_then(|st| {
                        chrono::DateTime::parse_from_rfc3339(&now)
                            .ok()
                            .map(|en| (en - st).num_seconds())
                    })
                    .filter(|s| *s >= 0)
                    .map(|s| format!(" {s}s"))
                    .unwrap_or_default();
                sp.ended_at = Some(now);
                println!("  {mark} {} {phase} {worker}{cost}{elapsed}", e.id);
            }
        }
        g.save(&step_home, &step_fleet);
    })
}
