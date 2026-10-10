//! P3b-§6.3 / D8: an aborted turn pauses with `kind: other`, the fixed
//! reason, and the gate wait it had accumulated.

use super::*;
use crate::cmd::fleet::review::constants::REVIEW_PAUSE_REASON_ABORTED;
use crate::cmd::fleet::review::schema::PauseKind;

/// Main answers; the reviewer's turn is aborted after the human sat at the
/// gate for `GATE_WAIT`. The gate wait is reported through the transport.
struct AbortReviewerTransport {
    main_sends: AtomicUsize,
    wait: Mutex<Duration>,
}

const GATE_WAIT: Duration = Duration::from_secs(120);

impl ReviewTransport for AbortReviewerTransport {
    fn send(&self, member: &str, _params: &serde_json::Value) -> anyhow::Result<String> {
        match member {
            "main" => {
                self.main_sends.fetch_add(1, Ordering::SeqCst);
                Ok("main output".to_string())
            }
            _ => Ok("REPLY-ABORTED".to_string()),
        }
    }
    fn turn_committed(&self) -> bool {
        // Only the reviewer's turn loses the race.
        self.main_sends.load(Ordering::SeqCst) == 0
    }
    fn take_human_wait(&self) -> Duration {
        // Reported on the reviewer's turn only.
        if self.main_sends.load(Ordering::SeqCst) == 0 {
            return Duration::ZERO;
        }
        std::mem::take(&mut *self.wait.lock().unwrap())
    }
}

#[test]
fn aborted_turn_pauses_kind_other_with_fixed_reason_and_wait() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let transport = AbortReviewerTransport {
        main_sends: AtomicUsize::new(0),
        wait: Mutex::new(GATE_WAIT),
    };

    let (ledger, stop) = run_review_loop(
        &transport,
        home,
        "review-x",
        &channel_id,
        "main",
        "reviewer",
        "task",
        Mode::Auto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
    )
    .unwrap();

    assert_eq!(
        stop,
        LoopDriverStop::Paused {
            reason: REVIEW_PAUSE_REASON_ABORTED.into()
        }
    );
    let payloads = read_payloads(home, &channel_id);
    let paused: Vec<_> = payloads
        .iter()
        .filter_map(|p| match p {
            ReviewPayload::Paused {
                kind,
                reason,
                human_wait_ms,
                ..
            } => Some((*kind, reason.as_str(), *human_wait_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(
        paused,
        [(
            PauseKind::Other,
            REVIEW_PAUSE_REASON_ABORTED,
            u64::try_from(GATE_WAIT.as_millis()).unwrap()
        )]
    );
    let reviewer_turns = payloads
        .iter()
        .filter(|p| {
            matches!(
                p,
                ReviewPayload::TurnSent {
                    to: Role::Reviewer,
                    ..
                }
            )
        })
        .count();
    assert_eq!(reviewer_turns, 0, "no turn_sent for the aborted reply");
    // `driver.rs` writes `paused` + `mode_changed` straight to the channel,
    // so the live ledger differs from replay in exactly those two fields.
    let mut live = ledger;
    live.paused = true;
    live.mode = Mode::SemiAuto;
    assert_eq!(fold_rounds(&payloads).unwrap(), live);
}
