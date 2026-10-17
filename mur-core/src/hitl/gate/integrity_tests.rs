//! Gate A reads the channel log through its integrity check (#1935, #1770).
//!
//! Signatures bind each line alone, so a deleted, truncated, or edited log
//! still verifies line by line. What does not survive is the hash chain and
//! the signed head in `channel-heads/`. The gate must treat a log that fails
//! either as untrustworthy: never release on it, ask again or deny.

use super::*;
use tempfile::TempDir;

fn action() -> ActionRequest {
    ActionRequest {
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({ "cmd": "rm -rf build" }),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        summary: "rm".into(),
    }
}

fn unattended() -> GatePolicy {
    GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: vec![],
    }
}

fn attended() -> GatePolicy {
    GatePolicy {
        yes: false,
        unanswered: Unanswered::Wait,
        auto_approve_tiers: vec![],
    }
}

/// A disk-keyed home (so every router append seals the head) and a channel.
fn setup() -> (TempDir, String) {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();
    (tmp, ch.id)
}

fn hash_of(ch: &str, a: &ActionRequest) -> String {
    action_hash(
        &a.tool_name,
        &a.tool_input,
        ch,
        &a.step_or_call_id,
        &a.agent_id,
    )
}

fn ago(d: chrono::Duration) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - d
}

fn append_router(
    home: &Path,
    ch: &str,
    actor: ChannelActor,
    kind: EventKind,
    payload: serde_json::Value,
) {
    let svc = ChannelService::open(home).unwrap();
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        ch,
        ROUTER_AGENT,
        actor,
        kind,
        payload,
        None,
    )
    .unwrap();
}

fn request_at(home: &Path, ch: &str, id: &str, hash: &str, at: chrono::DateTime<chrono::Utc>) {
    let q = HitlRequest {
        hitl_id: id.into(),
        action_hash: hash.into(),
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({}),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        timeout_ms: 0,
        summary: "x".into(),
        issued_at: Some(at),
    };
    append_router(
        home,
        ch,
        ChannelActor::System,
        EventKind::HitlRequest,
        serde_json::to_value(&q).unwrap(),
    );
}

fn decide_at(
    home: &Path,
    ch: &str,
    id: &str,
    hash: &str,
    allow: bool,
    at: chrono::DateTime<chrono::Utc>,
) {
    let r = HitlResponse {
        hitl_id: id.into(),
        action_hash: hash.into(),
        allow,
        reason: "test".into(),
        surface: "cli".into(),
        issued_at: Some(at),
        label: None,
    };
    append_router(
        home,
        ch,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&r).unwrap(),
    );
}

/// Allowed 2h ago (hitl-0), then asked again and DENIED 30m ago (hitl-1).
/// Untampered, the gate denies.
fn allow_then_deny(home: &Path, ch: &str, hash: &str) {
    request_at(home, ch, "hitl-0", hash, ago(chrono::Duration::hours(3)));
    decide_at(
        home,
        ch,
        "hitl-0",
        hash,
        true,
        ago(chrono::Duration::hours(2)),
    );
    request_at(home, ch, "hitl-1", hash, ago(chrono::Duration::hours(1)));
    decide_at(
        home,
        ch,
        "hitl-1",
        hash,
        false,
        ago(chrono::Duration::minutes(30)),
    );
}

fn log_path(home: &Path, ch: &str) -> std::path::PathBuf {
    ChannelService::open(home)
        .unwrap()
        .store()
        .events_path(ch)
        .unwrap()
}

/// Remove every log line `drop` matches, as a process that can write
/// `channels/` (but not `channel-heads/`) can.
fn drop_lines(home: &Path, ch: &str, drop: impl Fn(&mur_common::channel::ChannelEvent) -> bool) {
    let path = log_path(home, ch);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    for line in text.lines() {
        let ev: mur_common::channel::ChannelEvent = serde_json::from_str(line).unwrap();
        if !drop(&ev) {
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(&path, out).unwrap();
}

fn names(ev: &mur_common::channel::ChannelEvent, kind: EventKind, id: &str) -> bool {
    ev.kind == kind && ev.payload.get("hitl_id").and_then(|v| v.as_str()) == Some(id)
}

/// The signed head covers the first `lines` lines and no append is mid-seal.
fn sealed_through(home: &Path, ch: &str, lines: u64) -> bool {
    !mur_channel::anchor::is_dirty(home, ch)
        && mur_channel::head_io::read_head(&mur_channel::anchor::heads_dir(home), ch)
            .ok()
            .flatten()
            .is_some_and(|h| h.lines >= lines)
}

/// Either refusal is fail-closed. The decision must also say why.
fn assert_refused_for_integrity(r: Result<GateDecision>) {
    let d = r.expect("damage is a decision, not an error");
    assert!(!d.allow, "released on a tampered log: {d:?}");
    assert!(
        d.reason.contains("integrity"),
        "the reason must name the integrity failure: {d:?}"
    );
}

/// Precondition for the tamper tests: untouched, the history denies.
#[tokio::test]
async fn control_untampered_history_denies() {
    let (tmp, ch) = setup();
    let a = action();
    allow_then_deny(tmp.path(), &ch, &hash_of(&ch, &a));
    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow && !d.deferred, "the newer deny stands: {d:?}");
}

/// Case 1: the deny line alone is deleted. #1844 already refuses the allow
/// here (its request is left unanswered); the gate must now also see the
/// deletion itself and say so.
#[tokio::test]
async fn deleting_the_deny_line_is_caught_as_damage() {
    let (tmp, ch) = setup();
    let a = action();
    allow_then_deny(tmp.path(), &ch, &hash_of(&ch, &a));
    drop_lines(tmp.path(), &ch, |e| {
        names(e, EventKind::HitlResponse, "hitl-1")
    });

    assert_refused_for_integrity(gate(tmp.path(), &ch, &a, &unattended(), None, None).await);
}

/// Case 3: the log is cut back below the signed head, removing the later
/// request AND its deny. Nothing #1844 looks for is left: only the older
/// allow remains, and every remaining line verifies.
#[tokio::test]
async fn truncating_below_the_anchor_does_not_release_the_older_allow() {
    let (tmp, ch) = setup();
    let a = action();
    allow_then_deny(tmp.path(), &ch, &hash_of(&ch, &a));
    drop_lines(tmp.path(), &ch, |e| {
        names(e, EventKind::HitlRequest, "hitl-1") || names(e, EventKind::HitlResponse, "hitl-1")
    });

    assert_refused_for_integrity(gate(tmp.path(), &ch, &a, &unattended(), None, None).await);
}

/// Case 2: while the gate waits, its own request is deleted, so no answer can
/// ever bind to it. The gate must deny at once — not sit out the timeout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_the_gates_own_request_while_waiting_denies_at_once() {
    const WAIT: Duration = Duration::from_secs(20);
    const SEEN: Duration = Duration::from_secs(5);
    let (tmp, ch) = setup();
    let home = tmp.path().to_path_buf();
    let (h, c) = (home.clone(), ch.clone());
    let started = Instant::now();
    let waiting =
        tokio::spawn(async move { gate(&h, &c, &action(), &attended(), Some(WAIT), None).await });

    // Wait until the gate is waiting (request AND pause written), then
    // delete the request.
    let id = loop {
        let evs = ChannelService::open(&home)
            .unwrap()
            .load_events(&ch)
            .unwrap();
        // Sealed past the pause: deleting mid-seal would test the append's
        // own refusal, not the wait loop.
        let paused = evs.iter().any(|e| e.kind == EventKind::StateChange)
            && sealed_through(&home, &ch, evs.len() as u64);
        if let Some(q) = evs
            .iter()
            .filter(|e| e.kind == EventKind::HitlRequest)
            .find_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
            .filter(|_| paused)
        {
            break q.hitl_id;
        }
        assert!(started.elapsed() < SEEN, "the gate never wrote its request");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    drop_lines(&home, &ch, |e| names(e, EventKind::HitlRequest, &id));

    let r = waiting.await.unwrap();
    assert!(
        started.elapsed() < WAIT,
        "sat out the timeout instead of failing closed"
    );
    assert_refused_for_integrity(r);
}

/// A crash between the dirty marker and the head leaves the marker behind —
/// a deliberate false positive. The gate re-asks (a fresh request, not a
/// reused approval) and its own anchored write clears the marker.
#[tokio::test]
async fn a_dirty_marker_re_asks_instead_of_reusing_an_approval() {
    let (tmp, ch) = setup();
    let a = action();
    let hash = hash_of(&ch, &a);
    request_at(
        tmp.path(),
        &ch,
        "hitl-0",
        &hash,
        ago(chrono::Duration::hours(2)),
    );
    decide_at(
        tmp.path(),
        &ch,
        "hitl-0",
        &hash,
        true,
        ago(chrono::Duration::hours(1)),
    );
    mur_channel::anchor::mark_dirty(tmp.path(), &ch).unwrap();

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow && d.deferred, "re-asks: {d:?}");
    assert_ne!(d.hitl_id.as_deref(), Some("hitl-0"), "a fresh request");
    assert!(
        d.reason.contains("integrity"),
        "says why it asked again: {d:?}"
    );
    assert!(
        !mur_channel::anchor::is_dirty(tmp.path(), &ch),
        "the re-ask's anchored write clears the marker"
    );
}

/// A human approving while the gate polls is a concurrent anchored append —
/// mid-append the marker is down and the head trails the log. The gate's
/// snapshot must not read that as damage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_concurrent_approval_is_not_mistaken_for_damage() {
    const WAIT: Duration = Duration::from_secs(20);
    let (tmp, ch) = setup();
    let home = tmp.path().to_path_buf();
    let (h, c) = (home.clone(), ch.clone());
    let waiting =
        tokio::spawn(async move { gate(&h, &c, &action(), &attended(), Some(WAIT), None).await });

    let started = Instant::now();
    let q = loop {
        let evs = ChannelService::open(&home)
            .unwrap()
            .load_events(&ch)
            .unwrap();
        if let Some(q) = evs
            .iter()
            .filter(|e| e.kind == EventKind::HitlRequest)
            .find_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
        {
            break q;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "no request");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    // Busy the log with anchored appends while the gate polls, then answer.
    for _ in 0..20 {
        append_router(
            &home,
            &ch,
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({"text": "noise"}),
        );
    }
    decide_at(
        &home,
        &ch,
        &q.hitl_id,
        &q.action_hash,
        true,
        chrono::Utc::now(),
    );

    let d = waiting.await.unwrap().unwrap();
    assert!(d.allow, "a clean approval was refused: {d:?}");
}
