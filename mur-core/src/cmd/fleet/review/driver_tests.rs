//! Tests for `driver.rs` (D1): the transport seam and one turn of the
//! two-party protocol. Uses [`StubTransport`] — a test-only
//! [`ReviewTransport`] impl — never the real A2A wiring.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::driver::{ReviewTransport, TurnOutcome, run_turn};

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
