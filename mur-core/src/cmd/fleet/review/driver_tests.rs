//! Tests for `driver.rs` (D1): the transport seam and one turn of the
//! two-party protocol. Uses [`StubTransport`] — a test-only
//! [`ReviewTransport`] impl — never the real A2A wiring.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::driver::{RetryOutcome, ReviewTransport, TurnOutcome, run_turn, run_turn_with_retry};
use super::schema::{NoteClassification, ReviewPayload, classify_note_payload};

/// Test-only transport: counts sends and returns a fixed or queued reply,
/// never touching A2A.
struct StubTransport {
    sends: AtomicUsize,
    replies: Mutex<Vec<anyhow::Result<String>>>,
}

impl StubTransport {
    fn fixed(reply: &str) -> Self {
        Self {
            sends: AtomicUsize::new(0),
            replies: Mutex::new(vec![Ok(reply.to_string())]),
        }
    }

    fn queue(replies: Vec<anyhow::Result<String>>) -> Self {
        // Pop from the front in call order: reverse so `pop()` (back) yields
        // the first-queued reply first.
        let mut r = replies;
        r.reverse();
        Self {
            sends: AtomicUsize::new(0),
            replies: Mutex::new(r),
        }
    }

    fn send_count(&self) -> usize {
        self.sends.load(Ordering::SeqCst)
    }
}

impl ReviewTransport for StubTransport {
    fn send(&self, _member: &str, _params: &serde_json::Value) -> anyhow::Result<String> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.replies
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Ok(String::new()))
    }
}

fn stop_fleet(home: &std::path::Path, name: &str) {
    let dir = crate::cmd::fleet::store::fleet_dir(home, name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        crate::cmd::fleet::control::stopped_path(home, name),
        "stopped\n",
    )
    .unwrap();
}

/// A4: `.stopped` set BEFORE a turn ⇒ zero sends, outcome is `Stopped`.
#[test]
fn stopped_before_the_turn_sends_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    stop_fleet(home, "review-x");
    let transport = StubTransport::fixed("reply");
    let params = serde_json::json!({});

    let outcome = run_turn(&transport, home, "review-x", "reviewer", &params).unwrap();

    assert_eq!(outcome, TurnOutcome::Stopped);
    assert_eq!(
        transport.send_count(),
        0,
        "a stopped fleet must send nothing"
    );
}

/// Not stopped ⇒ exactly one send, and the reply flows through.
#[test]
fn not_stopped_sends_exactly_once() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let transport = StubTransport::fixed("hello from reviewer");
    let params = serde_json::json!({"message": "x"});

    let outcome = run_turn(&transport, home, "review-x", "reviewer", &params).unwrap();

    assert_eq!(
        outcome,
        TurnOutcome::Sent("hello from reviewer".to_string())
    );
    assert_eq!(transport.send_count(), 1);
}

/// A later stop does not retroactively affect a turn already sent, and a
/// second call after the stop is engaged sends nothing more — the pre-send
/// check is evaluated fresh on every call.
#[test]
fn stopping_between_two_calls_blocks_only_the_later_one() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let transport = StubTransport::queue(vec![Ok("first".to_string()), Ok("second".to_string())]);
    let params = serde_json::json!({});

    let first = run_turn(&transport, home, "review-x", "main", &params).unwrap();
    assert_eq!(first, TurnOutcome::Sent("first".to_string()));

    stop_fleet(home, "review-x");
    let second = run_turn(&transport, home, "review-x", "main", &params).unwrap();
    assert_eq!(second, TurnOutcome::Stopped);
    assert_eq!(transport.send_count(), 1, "only the first call sent");
}

/// A send failure propagates as an error rather than being swallowed; D2
/// builds the retry/pause behaviour on top of this.
#[test]
fn a_transport_error_propagates() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let transport = StubTransport::queue(vec![Err(anyhow::anyhow!("peer offline"))]);
    let params = serde_json::json!({});

    let err = run_turn(&transport, home, "review-x", "main", &params).unwrap_err();
    assert_eq!(err.to_string(), "peer offline");
}

/// Returns a fresh `~/.mur`-shaped tempdir with a review channel created and
/// the router's signing identity planted, so [`run_turn_with_retry`]'s
/// `write_paused_and_revert` has somewhere real to write.
fn setup_channel() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let channel_id = "review-ac14-channel".to_string();
    svc.store()
        .create(&mur_common::channel::Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: channel_id.clone(),
            title: "t".into(),
            goal: mur_common::channel::Goal::default(),
            state: mur_common::channel::ChannelState::Working,
            purpose: None,
            owner: mur_common::channel::ChannelActor::System,
            participants: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
        .unwrap();
    (tmp, channel_id)
}

/// AC14: send failure → exactly one retry (zero delay via injection, no real
/// sleeping) → still failing → a signed `paused` event with the reason, plus
/// mode reverted to semi-auto.
#[test]
fn ac14_two_failures_pause_with_reason_and_revert_to_semi_auto() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let transport = StubTransport::queue(vec![
        Err(anyhow::anyhow!("peer offline")),
        Err(anyhow::anyhow!("peer offline, retry also failed")),
    ]);
    let params = serde_json::json!({});

    let outcome = run_turn_with_retry(
        &transport,
        home,
        "review-x",
        "reviewer",
        &params,
        &channel_id,
        Duration::ZERO,
    )
    .unwrap();

    assert_eq!(
        transport.send_count(),
        2,
        "exactly one retry after the first failure"
    );
    let reason = match outcome {
        RetryOutcome::Paused { reason } => reason,
        other => panic!("expected Paused, got {other:?}"),
    };
    assert!(
        reason.contains("peer offline, retry also failed"),
        "reason should carry the retry's own failure text: {reason}"
    );

    let svc = mur_channel::ChannelService::open(home).unwrap();
    let events = svc.load_events(&channel_id).unwrap();
    let mut saw_paused = false;
    let mut saw_mode_changed = false;
    for ev in &events {
        if ev.kind != mur_common::channel::EventKind::Note {
            continue;
        }
        if let NoteClassification::Review(env) = classify_note_payload(&ev.payload) {
            match env.payload {
                ReviewPayload::Paused { reason: r, .. } => {
                    assert_eq!(r, reason);
                    saw_paused = true;
                }
                ReviewPayload::ModeChanged { mode } => {
                    assert_eq!(mode, super::schema::Mode::SemiAuto);
                    saw_mode_changed = true;
                }
                _ => {}
            }
        }
    }
    assert!(
        saw_paused,
        "a paused event with the reason must be on the channel"
    );
    assert!(saw_mode_changed, "mode must revert to semi-auto");
}

/// AC14 (success path): the first send succeeds ⇒ no retry, no paused event.
#[test]
fn ac14_first_send_success_means_no_retry_and_no_pause() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let transport = StubTransport::fixed("ok");
    let params = serde_json::json!({});

    let outcome = run_turn_with_retry(
        &transport,
        home,
        "review-x",
        "reviewer",
        &params,
        &channel_id,
        Duration::ZERO,
    )
    .unwrap();

    assert_eq!(transport.send_count(), 1);
    assert_eq!(outcome, RetryOutcome::Sent("ok".to_string()));
}

/// AC14 (recovers on retry): first send fails, the retry succeeds ⇒ no pause.
#[test]
fn ac14_retry_recovers_without_pausing() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let transport = StubTransport::queue(vec![
        Err(anyhow::anyhow!("transient")),
        Ok("recovered".to_string()),
    ]);
    let params = serde_json::json!({});

    let outcome = run_turn_with_retry(
        &transport,
        home,
        "review-x",
        "reviewer",
        &params,
        &channel_id,
        Duration::ZERO,
    )
    .unwrap();

    assert_eq!(transport.send_count(), 2);
    assert_eq!(outcome, RetryOutcome::Sent("recovered".to_string()));
}

/// A4 interacts with AC14: a stop observed on the retry attempt returns
/// `Stopped`, not `Paused` — a kill-switch stop is a distinct stop path.
#[test]
fn ac14_stop_during_retry_wins_over_pausing() {
    let (tmp, channel_id) = setup_channel();
    let home = tmp.path();
    let transport = StubTransport::queue(vec![Err(anyhow::anyhow!("first failure"))]);
    let params = serde_json::json!({});

    stop_fleet(home, "review-x");
    let outcome = run_turn_with_retry(
        &transport,
        home,
        "review-x",
        "reviewer",
        &params,
        &channel_id,
        Duration::ZERO,
    )
    .unwrap();

    assert_eq!(outcome, RetryOutcome::Stopped);
    assert_eq!(
        transport.send_count(),
        0,
        "stopped before the first send even happens"
    );
}

/// §5 semi-auto: a human who declines the send gate ends the turn as
/// `Stopped`, and nothing reaches the transport.
#[test]
fn declined_gate_sends_nothing() {
    struct Declining(AtomicUsize);
    impl ReviewTransport for Declining {
        fn send(&self, _m: &str, _p: &serde_json::Value) -> anyhow::Result<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("reply".into())
        }
        fn confirm_send(&self, _m: &str, _p: &serde_json::Value) -> anyhow::Result<bool> {
            Ok(false)
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let transport = Declining(AtomicUsize::new(0));
    let out = run_turn(
        &transport,
        tmp.path(),
        "review-x",
        "main",
        &serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(out, TurnOutcome::Stopped);
    assert_eq!(transport.0.load(Ordering::SeqCst), 0);
}
