//! A response settles a gate only as the answer to its own request (#1764,
//! fix items 2 and 3 as ruled in the issue thread).
//!
//! Approval of an *action* is reused for `APPROVAL_TTL_SECS` from the
//! response — that is the overnight case and stays. What is void is a
//! response that answers no router-signed request, answers a different
//! action than its request names, answers a request that was re-issued under
//! the same `hitl_id`, or lands after its request expired.

use super::*;
use tempfile::TempDir;

fn action(cmd: &str) -> ActionRequest {
    ActionRequest {
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({ "cmd": cmd }),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        summary: cmd.into(),
    }
}

/// Nobody is watching: park and report blocked.
fn unattended() -> GatePolicy {
    GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: vec![],
    }
}

/// A home with a router identity and one workflow channel.
fn setup() -> (TempDir, String) {
    let tmp = TempDir::new().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();
    (tmp, ch.id)
}

/// Run the gate unattended for `a` and return the parked request's
/// `(hitl_id, action_hash)`.
async fn park(home: &Path, ch: &str, a: &ActionRequest) -> (String, String) {
    let d = gate(home, ch, a, &unattended(), None, None).await.unwrap();
    assert!(!d.allow && d.deferred, "precondition: parked: {d:?}");
    (d.hitl_id.expect("parked id"), d.action_hash)
}

/// Append a router-signed event, as the router would.
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

/// The human approves `hitl_id` for `hash` — router-signed, `Human` actor.
fn human_allows(home: &Path, ch: &str, hitl_id: &str, hash: &str) {
    human_allows_at(home, ch, hitl_id, hash, Some(chrono::Utc::now()));
}

/// [`human_allows`] with an explicit signed `issued_at` (`None` = a payload
/// written before the field existed).
fn human_allows_at(
    home: &Path,
    ch: &str,
    hitl_id: &str,
    hash: &str,
    issued_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    human_decides_at(home, ch, hitl_id, hash, true, issued_at);
}

/// The human answers `hitl_id` with `allow` at signed time `issued_at`.
fn human_decides_at(
    home: &Path,
    ch: &str,
    hitl_id: &str,
    hash: &str,
    allow: bool,
    issued_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let resp = HitlResponse {
        hitl_id: hitl_id.into(),
        action_hash: hash.into(),
        allow,
        reason: "test".into(),
        surface: "cli".into(),
        issued_at,
    };
    append_router(
        home,
        ch,
        ChannelActor::Human {
            name: "user".into(),
        },
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
    );
}

/// A router-signed `HitlRequest` for `hitl_id`, as the gate writes it.
fn router_request(home: &Path, ch: &str, hitl_id: &str, hash: &str) {
    router_request_at(home, ch, hitl_id, hash, Some(chrono::Utc::now()));
}

/// [`router_request`] with an explicit signed `issued_at`.
fn router_request_at(
    home: &Path,
    ch: &str,
    hitl_id: &str,
    hash: &str,
    issued_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let q = HitlRequest {
        hitl_id: hitl_id.into(),
        action_hash: hash.into(),
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({}),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        timeout_ms: 0,
        summary: "x".into(),
        issued_at,
    };
    append_router(
        home,
        ch,
        ChannelActor::System,
        EventKind::HitlRequest,
        serde_json::to_value(&q).unwrap(),
    );
}

/// Set every event's store-assigned `ts` to `to`, as any process that can
/// write `channels/` can. `ts` is outside the signature, so every line still
/// verifies afterwards — which is why no HITL decision may read it (#1764 C).
fn rewrite_ts(home: &Path, ch: &str, to: chrono::DateTime<chrono::Utc>) {
    let svc = ChannelService::open(home).unwrap();
    let path = svc.store().events_path(ch);
    let text = std::fs::read_to_string(&path).unwrap();
    let mut out = String::new();
    for line in text.lines() {
        let mut ev: mur_common::channel::ChannelEvent = serde_json::from_str(line).unwrap();
        ev.ts = to;
        out.push_str(&serde_json::to_string(&ev).unwrap());
        out.push('\n');
    }
    std::fs::write(&path, out).unwrap();
}

fn ago(d: chrono::Duration) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - d
}

/// The `action_hash` the gate computes for `a` on `ch`.
fn hash_of(ch: &str, a: &ActionRequest) -> String {
    action_hash(
        &a.tool_name,
        &a.tool_input,
        ch,
        &a.step_or_call_id,
        &a.agent_id,
    )
}

fn ttl() -> chrono::Duration {
    chrono::Duration::seconds(mur_common::hitl::APPROVAL_TTL_SECS)
}

// ── Controls: what must keep working ──────────────────────────────────

/// The overnight case: a human answers run 1's request, run 2 of the same
/// action — a new `hitl_id` — is released by that answer.
#[tokio::test]
async fn a_new_run_of_an_approved_action_reuses_the_approval() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    human_allows(tmp.path(), &ch, &id, &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(d.allow, "a re-run is a new instance, not a re-issue: {d:?}");
}

/// A late but in-time answer (day 6 of 7) still counts.
#[tokio::test]
async fn an_answer_inside_the_request_window_counts() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let hash = hash_of(&ch, &a);
    router_request_at(
        tmp.path(),
        &ch,
        "hitl-old",
        &hash,
        Some(ago(ttl() - chrono::Duration::hours(1))),
    );
    human_allows(tmp.path(), &ch, "hitl-old", &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(d.allow, "answered before the request expired: {d:?}");
}

// ── Binding (fix item 2) ──────────────────────────────────────────────

/// An answer whose `hitl_id` names no request settles nothing.
#[tokio::test]
async fn an_answer_to_no_request_does_not_settle() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    human_allows(tmp.path(), &ch, "hitl-ghost", &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "no request named hitl-ghost: {d:?}");
    assert_eq!(
        d.hitl_id.as_deref(),
        Some(id.as_str()),
        "the real request is still pending"
    );
}

/// An answer that precedes its request answers nothing: nobody had been
/// asked yet.
#[tokio::test]
async fn an_answer_before_its_request_does_not_settle() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (_, hash) = park(tmp.path(), &ch, &a).await;
    // Signed times, not line order: line order is not signed (#1770).
    router_request(tmp.path(), &ch, "hitl-early", &hash);
    human_allows_at(
        tmp.path(),
        &ch,
        "hitl-early",
        &hash,
        Some(ago(chrono::Duration::hours(1))),
    );

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "answered before it was asked: {d:?}");
}

/// The answer's `action_hash` must be its request's. Otherwise approving a
/// harmless request could carry the hash of something else.
#[tokio::test]
async fn an_answer_whose_hash_differs_from_its_request_does_not_settle() {
    let (tmp, ch) = setup();
    let harmless = action("ls");
    let target = action("rm -rf build");
    let (harmless_id, _) = park(tmp.path(), &ch, &harmless).await;
    let (_, target_hash) = park(tmp.path(), &ch, &target).await;
    human_allows(tmp.path(), &ch, &harmless_id, &target_hash);

    let d = gate(tmp.path(), &ch, &target, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "the human saw `ls`, not `rm`: {d:?}");
}

// ── Re-issue and expiry (fix item 3) ──────────────────────────────────

/// A second router-signed request under the same `hitl_id` is a bug or a
/// forgery; every answer to that id is void.
#[tokio::test]
async fn a_reissued_hitl_id_voids_its_answers() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    human_allows(tmp.path(), &ch, &id, &hash);
    router_request(tmp.path(), &ch, &id, &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "re-issued id: {d:?}");
    assert_ne!(
        d.hitl_id.as_deref(),
        Some(id.as_str()),
        "a poisoned id is never offered for answering again"
    );
}

/// An answer that lands after its request expired does not count.
#[tokio::test]
async fn an_answer_after_the_request_expired_does_not_settle() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let hash = hash_of(&ch, &a);
    let id = "hitl-expired";
    router_request_at(
        tmp.path(),
        &ch,
        id,
        &hash,
        Some(ago(ttl() + chrono::Duration::hours(1))),
    );
    human_allows(tmp.path(), &ch, id, &hash);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow && d.deferred, "request had expired: {d:?}");
    assert_ne!(d.hitl_id.as_deref(), Some(id), "asks afresh");
}

// ── Signed time only (#1764 option C) ─────────────────────────────────

/// Rewriting the unsigned `ts` cannot revive an expired approval: freshness
/// is read from the signed `issued_at`, and every line still verifies after
/// the rewrite, so a `ts`-based check would have been fooled.
#[tokio::test]
async fn rewriting_ts_does_not_revive_an_expired_approval() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let hash = hash_of(&ch, &a);
    let old = ago(ttl() + chrono::Duration::days(1));
    router_request_at(tmp.path(), &ch, "hitl-stale", &hash, Some(old));
    human_allows_at(
        tmp.path(),
        &ch,
        "hitl-stale",
        &hash,
        Some(old + chrono::Duration::minutes(5)),
    );
    rewrite_ts(tmp.path(), &ch, chrono::Utc::now());

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "a week-old approval with a fresh ts: {d:?}");
}

/// A response with no signed `issued_at` predates option C. Its issue time
/// is unknown, so it settles nothing (fail closed).
#[tokio::test]
async fn a_legacy_answer_without_issued_at_does_not_settle() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let (id, hash) = park(tmp.path(), &ch, &a).await;
    human_allows_at(tmp.path(), &ch, &id, &hash, None);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow, "issue time unknown: {d:?}");
}

/// A legacy request (no `issued_at`) can never be validly answered, so it is
/// never offered as the pending one: the gate asks afresh instead of parking
/// on a question nobody can settle.
#[tokio::test]
async fn a_legacy_request_is_replaced_not_left_pending() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let hash = hash_of(&ch, &a);
    router_request_at(tmp.path(), &ch, "hitl-legacy", &hash, None);

    let d = gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap();
    assert!(!d.allow && d.deferred, "{d:?}");
    assert_ne!(d.hitl_id.as_deref(), Some("hitl-legacy"), "asks afresh");
}

/// `mur channel approve` / the phone refuse a request that can no longer be
/// validly answered, rather than writing an answer the gate will ignore.
#[tokio::test]
async fn an_unanswerable_request_is_refused_at_approve_time() {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let hash = hash_of(&ch, &a);
    router_request_at(tmp.path(), &ch, "hitl-legacy", &hash, None);
    router_request_at(
        tmp.path(),
        &ch,
        "hitl-expired",
        &hash,
        Some(ago(ttl() + chrono::Duration::hours(1))),
    );
    let evs = ChannelService::open(tmp.path())
        .unwrap()
        .load_events(&ch)
        .unwrap();

    for id in ["hitl-legacy", "hitl-expired"] {
        let r = crate::hitl::authority::request_to_answer(tmp.path(), &ch, &evs, id);
        assert!(r.is_err(), "{id} must not be answerable");
    }
}

// ── Ordering of settled decisions (#1772) ─────────────────────────────
//
// Settled decisions are ordered by signed `issued_at`: the newest wins, and
// on an equal `issued_at` a deny wins. Line order is unsigned and never
// decides, so each rule is checked with the lines both ways round.

/// Two human answers to `a`, written to a fresh channel in the given line
/// order; returns the gate's decision. Each answer has its own request,
/// signed before either answer.
async fn settle(lines: [(bool, chrono::DateTime<chrono::Utc>); 2]) -> GateDecision {
    let (tmp, ch) = setup();
    let a = action("rm -rf build");
    let hash = hash_of(&ch, &a);
    let asked = ago(chrono::Duration::hours(3));
    for (i, (allow, at)) in lines.into_iter().enumerate() {
        let id = format!("hitl-{i}");
        router_request_at(tmp.path(), &ch, &id, &hash, Some(asked));
        human_decides_at(tmp.path(), &ch, &id, &hash, allow, Some(at));
    }
    gate(tmp.path(), &ch, &a, &unattended(), None, None)
        .await
        .unwrap()
}

/// The newest signed decision wins whichever line it is on, and it wins
/// for an allow as well as a deny — this is not "a deny always wins".
#[tokio::test]
async fn the_newest_signed_decision_wins_in_either_line_order() {
    let older = ago(chrono::Duration::hours(2));
    let newer = ago(chrono::Duration::hours(1));

    let d = settle([(false, newer), (true, older)]).await;
    assert!(!d.allow && !d.deferred, "newer deny listed first: {d:?}");
    let d = settle([(true, older), (false, newer)]).await;
    assert!(!d.allow && !d.deferred, "newer deny listed last: {d:?}");
    let d = settle([(true, newer), (false, older)]).await;
    assert!(d.allow, "newer allow listed first: {d:?}");
    let d = settle([(false, older), (true, newer)]).await;
    assert!(d.allow, "newer allow listed last: {d:?}");
}

/// Equal signed `issued_at` — one value, written into both payloads, not two
/// clock reads that might differ — and a deny wins, whichever line it is on.
#[tokio::test]
async fn a_deny_wins_a_tie_on_signed_time() {
    let same = ago(chrono::Duration::hours(1));

    let d = settle([(true, same), (false, same)]).await;
    assert!(!d.allow && !d.deferred, "allow then deny: {d:?}");
    let d = settle([(false, same), (true, same)]).await;
    assert!(!d.allow && !d.deferred, "deny then allow: {d:?}");
}
