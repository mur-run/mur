//! #1935: the gate reads prior decisions through the checked loader, so a log
//! whose chain or signed head no longer holds never releases a gate.
//!
//! The attack: allow an action, later deny it, then delete the deny (and its
//! request) from `events.jsonl`. Without the check, the older allow is all
//! that is left and `scan_prior` replays it without asking anyone.

use super::*;
use tempfile::TempDir;

fn destructive() -> ActionRequest {
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

/// Answer `hitl_id` as the local human, signed and anchored by the router.
fn answer(home: &Path, ch: &str, hitl_id: &str, allow: bool) {
    let svc = ChannelService::open(home).unwrap();
    let req: HitlRequest = svc
        .load_events(ch)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .filter_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
        .find(|r| r.hitl_id == hitl_id)
        .expect("request exists");
    let resp = HitlResponse {
        hitl_id: req.hitl_id,
        action_hash: req.action_hash,
        allow,
        reason: "test".into(),
        surface: "cli".into(),
        issued_at: Some(chrono::Utc::now()),
        label: None,
    };
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        ch,
        ROUTER_AGENT,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
        None,
    )
    .unwrap();
}

async fn park(home: &Path, ch: &str) -> String {
    let d = gate(home, ch, &destructive(), &unattended(), None, None)
        .await
        .unwrap();
    assert!(d.deferred, "expected a parked request: {d:?}");
    d.hitl_id.expect("parked id")
}

fn log_path(home: &Path, ch: &str) -> std::path::PathBuf {
    home.join("channels").join(ch).join("events.jsonl")
}

/// Drop every log line mentioning `needle`.
fn delete_lines(home: &Path, ch: &str, needle: &str) {
    let path = log_path(home, ch);
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: String = text
        .lines()
        .filter(|l| !l.contains(needle))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(kept.len(), text.len(), "nothing matched {needle}");
    std::fs::write(&path, kept).unwrap();
}

/// allow → deny on the same action. Returns the deny's request id.
async fn allow_then_deny(home: &Path, ch: &str) -> String {
    let first = park(home, ch).await;
    answer(home, ch, &first, true);
    // The allow is live: a run in this state proceeds.
    let d = gate(home, ch, &destructive(), &unattended(), None, None)
        .await
        .unwrap();
    assert!(d.allow, "precondition: the human's allow releases: {d:?}");

    // Expire the allow's relevance by a newer request + deny. `scan_prior`
    // would replay the allow, so park the second request directly.
    let svc = ChannelService::open(home).unwrap();
    let second = format!("hitl-{}", uuid::Uuid::now_v7());
    let req = HitlRequest {
        hitl_id: second.clone(),
        action_hash: d.action_hash.clone(),
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({ "cmd": "rm -rf build" }),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        timeout_ms: 0,
        summary: "rm".into(),
        issued_at: Some(chrono::Utc::now()),
    };
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        ch,
        ROUTER_AGENT,
        ChannelActor::System,
        EventKind::HitlRequest,
        serde_json::to_value(&req).unwrap(),
        None,
    )
    .unwrap();
    drop(svc);
    answer(home, ch, &second, false);
    let d = gate(home, ch, &destructive(), &unattended(), None, None)
        .await
        .unwrap();
    assert!(
        !d.allow && !d.deferred,
        "precondition: the newer deny holds: {d:?}"
    );
    second
}

#[tokio::test]
async fn deleting_a_newer_deny_does_not_revive_the_older_allow() {
    let tmp = TempDir::new().unwrap();
    let _router = crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();
    let denied = allow_then_deny(tmp.path(), &ch.id).await;

    // Remove the deny and the request it answered: what is left is a
    // well-formed, fully signed log whose newest word is "allow".
    delete_lines(tmp.path(), &ch.id, &denied);

    let d = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &unattended(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!d.allow, "a tampered log released the gate: {d:?}");
    assert!(d.deferred, "the gate must ask again, not refuse: {d:?}");
    assert!(
        d.reason.contains("integrity"),
        "the parked request should say why earlier answers were ignored: {}",
        d.reason
    );
    // The fresh request is on disk for a human to answer.
    let id = d.hitl_id.expect("parked id");
    let log = std::fs::read_to_string(log_path(tmp.path(), &ch.id)).unwrap();
    assert!(log.contains(&id), "re-ask request was not written");
}

/// Cutting the newest line (the deny) off the end leaves a chain that is
/// still intact; only the signed head can tell the log got shorter.
#[tokio::test]
async fn a_truncated_tail_is_caught_by_the_signed_head() {
    let tmp = TempDir::new().unwrap();
    let _router = crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();
    let id = park(tmp.path(), &ch.id).await;
    answer(tmp.path(), &ch.id, &id, false);

    let path = log_path(tmp.path(), &ch.id);
    let text = std::fs::read_to_string(&path).unwrap();
    // The answer is the newest line naming this request; keep everything
    // before it, so the remaining prefix still chains cleanly.
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .rposition(|l| l.contains(&id) && l.contains("hitl-response"))
        .expect("the deny is in the log");
    std::fs::write(&path, lines[..at].join("\n") + "\n").unwrap();

    let damage = super::checked::load(tmp.path(), &ch.id).unwrap().damage;
    assert!(
        damage.as_deref().is_some_and(|d| d.contains("Truncated")),
        "the loader must report the truncation: {damage:?}"
    );
    let d = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &unattended(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!d.allow && d.deferred, "must ask again: {d:?}");
}

/// The control: an untouched log with a head keeps working exactly as before.
#[tokio::test]
async fn an_intact_anchored_log_still_replays_the_human_decision() {
    let tmp = TempDir::new().unwrap();
    let _router = crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();
    let id = park(tmp.path(), &ch.id).await;
    answer(tmp.path(), &ch.id, &id, true);
    assert_eq!(
        super::checked::load(tmp.path(), &ch.id).unwrap().damage,
        None
    );
    let d = gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &unattended(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(d.allow, "an intact log must still release: {d:?}");
}
