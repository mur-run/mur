//! #1776: every turn-ending event carries the session's running execution
//! time (wall time minus human wait), not a placeholder zero.

use super::*;

/// Semi-auto stub on a fake clock: each send waits `gate_wait` at the human
/// gate (reported as human wait), then runs for `exec`. The reviewer answers
/// from `verdicts` in order.
struct TimedTransport {
    clock: Rc<Cell<Instant>>,
    gate_wait: Duration,
    exec: Duration,
    waited: Cell<Duration>,
    verdicts: Mutex<Vec<String>>,
}

impl ReviewTransport for TimedTransport {
    fn confirm_send(
        &self,
        _member: &str,
        _params: &serde_json::Value,
        _open: &std::collections::BTreeSet<String>,
    ) -> anyhow::Result<crate::cmd::fleet::review::driver::SendAnswer> {
        self.clock.set(self.clock.get() + self.gate_wait);
        self.waited.set(self.waited.get() + self.gate_wait);
        Ok(crate::cmd::fleet::review::driver::SendAnswer::Send)
    }

    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        self.clock.set(self.clock.get() + self.exec);
        match member {
            "main" => Ok(with_accept_all("main output", params)),
            "reviewer" => Ok(self.verdicts.lock().unwrap().remove(0)),
            other => panic!("unexpected member {other:?}"),
        }
    }

    fn take_human_wait(&self) -> Duration {
        self.waited.take()
    }
}

#[test]
fn verdicts_and_rebuttals_carry_execution_time_without_human_wait() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let clock = Rc::new(Cell::new(Instant::now()));
    let transport = TimedTransport {
        clock: Rc::clone(&clock),
        gate_wait: Duration::from_secs(60),
        exec: Duration::from_millis(2500),
        waited: Cell::new(Duration::ZERO),
        verdicts: Mutex::new(vec![
            serde_json::json!({
                "verdict": "revise",
                "findings": [{"severity": "low", "issue": "needs a doc comment"}],
            })
            .to_string(),
            serde_json::json!({
                "verdict": "approve",
                "prior": [{"id": "F1", "status": "resolved"}],
            })
            .to_string(),
        ]),
    };

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(600), Stuck::Off, None),
        &|| clock.get(),
        None,
    )
    .unwrap();

    assert_eq!(stop, LoopDriverStop::Approve);
    let stamped: Vec<(&str, u64)> = read_payloads(home, &channel_id)
        .iter()
        .filter_map(|p| match p {
            ReviewPayload::Verdict { cumulative, .. } => Some(("verdict", cumulative.exec_time_ms)),
            ReviewPayload::Rebuttal { cumulative, .. } => {
                Some(("rebuttal", cumulative.exec_time_ms))
            }
            _ => None,
        })
        .collect();
    // 2.5 s per turn; the 60 s at each gate is not execution time.
    assert_eq!(
        stamped,
        [("verdict", 5_000), ("rebuttal", 7_500), ("verdict", 10_000)]
    );
    // `session_stopped` is built from this ledger's total.
    assert_eq!(ledger.exec_time_ms, 10_000);
}
