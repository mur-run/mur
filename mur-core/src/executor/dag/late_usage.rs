//! What a delegated turn spent when the `channel/delegate` dial failed
//! (#2083, #2090).
//!
//! The member records a turn's usage only once the turn ends, and `tasks/get`
//! returns it from then on. A dial that fails while the turn is still running
//! — the likeliest shape of an io timeout — used to read a usage-less
//! `working` task once and count the step as free, while the turn went on
//! billing. Now the unwanted turn is cancelled (its reply can never reach the
//! run) and its final usage is waited for, bounded by the run's deadline.
//! When it still cannot be learned, the step says so rather than reporting a
//! silent zero.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;

use super::TokenUsage;

/// The longest a failed step waits for its member's turn to end. A cancelled
/// turn ends at its next yield point, normally within seconds; this only
/// bounds a member that ignores the cancel. The run's own deadline, when
/// nearer, wins.
pub(super) const LATE_USAGE_WAIT: Duration = Duration::from_secs(120);

/// How often the member is asked again while its turn winds down.
const LATE_USAGE_POLL: Duration = Duration::from_millis(500);

/// The runtime's `TaskNotFound` code: the member never registered the turn.
const TASK_NOT_FOUND_CODE: &str = "\"code\":-32000";

/// What the member's turn is known to have spent.
#[derive(Debug)]
pub(super) enum Recovered {
    /// The turn ended and reported this usage — zero only when the member
    /// has no record of it at all, i.e. it never ran.
    Known(TokenUsage),
    /// The spend could not be learned; the reason is for the operator.
    Unknown(String),
}

/// One `tasks/get` answer, reduced to what accounting needs.
enum Probe {
    NotFound,
    Running,
    Ended(Option<TokenUsage>),
}

/// What the delegation `task_id` on `member` spent, waiting at most `wait`
/// for a turn that is still running. A running turn is cancelled first: its
/// reply was lost with the dial, so finishing it only costs more.
pub(super) async fn recover_delegate_usage(
    mur_home: &Path,
    member: &str,
    task_id: &str,
    wait: Duration,
) -> Recovered {
    let until = Instant::now() + wait;
    let mut cancelled = false;
    loop {
        match probe(mur_home, member, task_id).await {
            Err(error) => {
                tracing::warn!(member, task_id, %error, "delegate usage: tasks/get failed");
                return Recovered::Unknown(format!("tasks/get failed: {error:#}"));
            }
            Ok(Probe::NotFound) => return Recovered::Known(TokenUsage::default()),
            Ok(Probe::Ended(Some(usage))) => return Recovered::Known(usage),
            Ok(Probe::Ended(None)) => {
                return Recovered::Unknown(
                    "the member's runtime does not report a finished turn's usage \
                     (restart it on a current build)"
                        .into(),
                );
            }
            Ok(Probe::Running) => {
                if !cancelled {
                    cancelled = true;
                    if let Err(error) = call(mur_home, member, "tasks/cancel", task_id).await {
                        tracing::warn!(member, task_id, %error, "delegate usage: tasks/cancel failed");
                    }
                }
                if Instant::now() + LATE_USAGE_POLL > until {
                    return Recovered::Unknown(format!(
                        "the turn was still running after {}s",
                        wait.as_secs()
                    ));
                }
                tokio::time::sleep(LATE_USAGE_POLL).await;
            }
        }
    }
}

/// How long a failed step may wait: [`LATE_USAGE_WAIT`], or less when the
/// run's deadline is nearer.
pub(super) fn wait_budget(deadline_at: Option<Instant>, now: Instant) -> Duration {
    match deadline_at {
        Some(d) => d.saturating_duration_since(now).min(LATE_USAGE_WAIT),
        None => LATE_USAGE_WAIT,
    }
}

async fn probe(mur_home: &Path, member: &str, task_id: &str) -> Result<Probe> {
    match call(mur_home, member, "tasks/get", task_id).await {
        Ok(task) => Ok(classify(&task)),
        Err(e) if format!("{e:#}").contains(TASK_NOT_FOUND_CODE) => Ok(Probe::NotFound),
        Err(e) => Err(e),
    }
}

fn classify(task: &Value) -> Probe {
    match task.get("state").and_then(Value::as_str) {
        Some("submitted" | "working") => Probe::Running,
        _ => Probe::Ended(task.get("usage").map(|_| super::extract_usage(task))),
    }
}

async fn call(mur_home: &Path, member: &str, method: &'static str, task_id: &str) -> Result<Value> {
    let home = mur_home.to_path_buf();
    let (m, id) = (member.to_string(), task_id.to_string());
    tokio::task::spawn_blocking(move || {
        crate::a2a_dial::dial_method(
            &home,
            &m,
            method,
            serde_json::json!({ "id": id }),
            crate::a2a_dial::DialMode::RequireRunning,
        )
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("{method} task failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_is_capped_by_the_run_deadline() {
        let now = Instant::now();
        assert_eq!(wait_budget(None, now), LATE_USAGE_WAIT);
        assert_eq!(
            wait_budget(Some(now + Duration::from_secs(5)), now),
            Duration::from_secs(5)
        );
        assert_eq!(
            wait_budget(Some(now + LATE_USAGE_WAIT * 10), now),
            LATE_USAGE_WAIT
        );
        assert_eq!(wait_budget(Some(now), now), Duration::ZERO);
    }

    #[test]
    fn only_an_ended_turn_with_usage_counts_as_known() {
        let running = serde_json::json!({"state": "working"});
        assert!(matches!(classify(&running), Probe::Running));
        let old = serde_json::json!({"state": "completed"});
        assert!(matches!(classify(&old), Probe::Ended(None)));
        let done = serde_json::json!({"state": "failed",
            "usage": {"input_tokens": 10, "output_tokens": 2}});
        match classify(&done) {
            Probe::Ended(Some(u)) => assert_eq!(u.total(), 12),
            _ => panic!("an ended turn with usage is known"),
        }
    }
}
