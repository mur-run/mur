use super::*;
use tempfile::TempDir;

fn req(tier: RiskTier) -> ActionRequest {
    ActionRequest {
        tier,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({ "cmd": "echo hi" }),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        summary: "echo".into(),
    }
}

#[tokio::test]
async fn read_tier_runs_unattended() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    let d = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Read),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Wait,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-g"),
    )
    .await
    .unwrap();
    assert!(d.allow);
}

/// The gate's two state transitions must carry the run that paused, or a
/// rebuild — which filters BY `run_id` — cannot see them. A run whose cache
/// is lost while it waits on an approval would then rebuild as `Working`,
/// contradicting the channel about the one state the operator came to see.
#[tokio::test]
async fn gate_transitions_are_stamped_with_the_run_that_paused() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    // `yes` auto-approves, so the gate runs to completion and writes BOTH
    // transitions: into `input-required` and back out to `working`.
    gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: true,
            unanswered: Unanswered::Wait,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-paused"),
    )
    .await
    .unwrap();

    let svc = ChannelService::open(tmp.path()).unwrap();
    let stamped: Vec<String> = svc
        .load_events(&ch.id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::StateChange)
        .filter(|e| {
            e.payload
                .get("run_id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == "run-paused")
        })
        .filter_map(|e| {
            e.payload
                .get("to")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();

    assert!(
        stamped.iter().any(|to| to == "input-required"),
        "the pause transition was not attributed to the run: {stamped:?}"
    );
    assert!(
        stamped.iter().any(|to| to == "working"),
        "the resume transition was not attributed to the run: {stamped:?}"
    );
}

#[tokio::test]
async fn high_tier_approved_via_prewritten_response() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    let r = req(RiskTier::Destructive);
    let hash = action_hash(
        &r.tool_name,
        &r.tool_input,
        &ch.id,
        &r.step_or_call_id,
        &r.agent_id,
    );
    let d = gate(
        tmp.path(),
        &ch.id,
        &r,
        &GatePolicy {
            yes: true,
            unanswered: Unanswered::Wait,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-g"),
    )
    .await
    .unwrap();
    assert!(d.allow, "--yes auto-approves a high tier");
    assert_eq!(d.action_hash, hash);
    // Check the trail via a fresh open.
    let svc2 = ChannelService::open(tmp.path()).unwrap();
    let kinds: Vec<_> = svc2
        .load_events(&ch.id)
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&EventKind::HitlRequest));
    assert!(kinds.contains(&EventKind::HitlResponse));
}

#[tokio::test]
async fn drift_denies_fail_closed() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    let r = req(RiskTier::Spend);
    let resp = HitlResponse {
        hitl_id: "h-x".into(),
        action_hash: "WRONGHASH".into(),
        allow: true,
        reason: "".into(),
        surface: "cli".into(),
    };
    // Router-signed, so it reaches the hash check: an unsigned one would
    // be ignored and the test would time out instead of drifting.
    let router = plant_router_identity(tmp.path());
    svc.append_signed(
        &ch.id,
        &router,
        0,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
        None,
    )
    .unwrap();
    // Drop svc before waiting (don't hold across await).
    drop(svc);
    let d = wait_for_response(
        tmp.path(),
        &ch.id,
        "h-x",
        "EXPECTED",
        std::time::Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert!(!d.allow, "mismatched action_hash must fail-closed");
    assert!(d.reason.contains("drift"));
    let _ = r;
}

use mur_common::identity::AgentIdentity;

/// Plant a router identity at `<home>/agents/mur/` and return it.
///
/// Delegates to the shared fixture so there is ONE definition of "a home
/// whose router can sign" — a second, drifting copy is how some tests in
/// this file ended up signing while others silently did not.
fn plant_router_identity(home: &Path) -> AgentIdentity {
    crate::channel_writer::plant_writer_identity(home)
}

fn resp_with_hash(hitl_id: &str, hash: &str) -> HitlResponse {
    HitlResponse {
        hitl_id: hitl_id.into(),
        action_hash: hash.into(),
        allow: true,
        reason: "".into(),
        surface: "cli".into(),
    }
}

/// A correctly-signed HitlResponse from the router releases the gate.
#[tokio::test]
async fn router_signed_response_releases() {
    let tmp = TempDir::new().unwrap();
    let id = plant_router_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    let resp = resp_with_hash("h-ok", "EXPECTED");
    svc.append_signed(
        &ch.id,
        &id,
        0,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
        None,
    )
    .unwrap();
    drop(svc);
    let d = wait_for_response(
        tmp.path(),
        &ch.id,
        "h-ok",
        "EXPECTED",
        std::time::Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert!(
        d.allow,
        "router-signed response with matching hash releases"
    );
}

/// A response signed by a NON-router (attacker) key must NOT release the
/// gate — a present-but-invalid signature is always rejected, regardless of
/// `MUR_CHANNEL_REQUIRE_SIG`.
#[tokio::test]
async fn forged_signature_does_not_release() {
    let tmp = TempDir::new().unwrap();
    let _router = plant_router_identity(tmp.path());
    let attacker = AgentIdentity::generate();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    let resp = resp_with_hash("h-forge", "EXPECTED");
    // Signed by the attacker, not the router → verify_one rejects it.
    svc.append_signed(
        &ch.id,
        &attacker,
        0,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
        None,
    )
    .unwrap();
    drop(svc);
    let d = wait_for_response(
        tmp.path(),
        &ch.id,
        "h-forge",
        "EXPECTED",
        std::time::Duration::from_millis(900),
    )
    .await
    .unwrap();
    assert!(
        !d.allow,
        "a forged (wrong-key) signature must never release the gate"
    );
    assert!(d.reason.contains("timeout"), "ignored → waits → times out");
}

/// An UNSIGNED response never releases the gate, whatever
/// `MUR_CHANNEL_REQUIRE_SIG` says — it is off by default, and a sandboxed
/// `mur channel approve` (which cannot read the router key) writes exactly
/// this. The gate no longer reads that variable, so nothing here sets it.
#[tokio::test]
async fn unsigned_response_does_not_release() {
    let tmp = TempDir::new().unwrap();
    let _router = plant_router_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    let resp = resp_with_hash("h-unsigned", "EXPECTED");
    svc.append(
        &ch.id,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&resp).unwrap(),
        None,
    )
    .unwrap();
    drop(svc);
    let d = wait_for_response(
        tmp.path(),
        &ch.id,
        "h-unsigned",
        "EXPECTED",
        std::time::Duration::from_millis(900),
    )
    .await
    .unwrap();
    assert!(!d.allow, "an unsigned response must never release the gate");
    assert!(d.reason.contains("timeout"), "ignored → waits → times out");
}

/// The self-approval, end to end. The agent whose action is gated reads
/// the parked request, echoes its `action_hash`, and signs the answer
/// with its OWN key — a signature `verify_event` accepts. The gate must
/// stay parked: a verified signature is not an authorization.
#[tokio::test]
async fn an_agent_cannot_approve_its_own_gated_action() {
    let tmp = TempDir::new().unwrap();
    let _router = plant_router_identity(tmp.path());
    let qa = crate::channel_writer::plant_identity_for(tmp.path(), "qa");
    let ch = ChannelService::open(tmp.path())
        .unwrap()
        .create_for_workflow("g")
        .unwrap();
    let policy = GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: vec![],
    };
    let mut r = req(RiskTier::Destructive);
    r.agent_id = "qa".into();
    let parked = gate(tmp.path(), &ch.id, &r, &policy, None, Some("run-1"))
        .await
        .unwrap();
    let hitl_id = parked.hitl_id.clone().expect("parked");

    let svc = ChannelService::open(tmp.path()).unwrap();
    let self_approval = svc
        .append_signed(
            &ch.id,
            &qa,
            0,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::HitlResponse,
            serde_json::to_value(resp_with_hash(&hitl_id, &parked.action_hash)).unwrap(),
            None,
        )
        .unwrap();
    // And the same thing unsigned, claiming to be the human.
    svc.append(
        &ch.id,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(resp_with_hash(&hitl_id, &parked.action_hash)).unwrap(),
        None,
    )
    .unwrap();
    drop(svc);
    assert!(
        crate::channel_verify::verify_event(tmp.path(), &ch.id, &self_approval, false),
        "precondition: the agent's signature verifies"
    );

    let d = gate(tmp.path(), &ch.id, &r, &policy, None, Some("run-2"))
        .await
        .unwrap();
    assert!(!d.allow, "the agent approved itself: {d:?}");
    assert!(d.deferred, "the request stays parked for the human");
    assert_eq!(d.hitl_id.as_deref(), Some(hitl_id.as_str()));

    // The human's answer, through the real command, still releases it.
    crate::cmd::channel::approve_in(tmp.path(), &ch.id, &hitl_id, false, None).unwrap();
    let d = gate(tmp.path(), &ch.id, &r, &policy, None, Some("run-3"))
        .await
        .unwrap();
    assert!(d.allow, "the router-signed human answer releases: {d:?}");
}

// ── Defer / durable-approval behaviour (unattended HITL, P0) ────────────

/// Read back the ids of every HitlRequest in a channel, oldest first.
fn pending_request_ids(home: &Path, ch: &str) -> Vec<String> {
    let svc = ChannelService::open(home).unwrap();
    svc.load_events(ch)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .filter_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
        .map(|r| r.hitl_id)
        .collect()
}

/// Answer a parked request the way `mur channel approve` does: echo the
/// request's own `action_hash` so the pin re-verify passes.
/// Answer a parked request the way `mur channel approve` does — SIGNED by
/// the router (`append_as_writer`), not through a bare
/// `ChannelService::append`. Under `MUR_CHANNEL_REQUIRE_SIG=1` the gate
/// drops an unsigned `HitlResponse`, so an unsigned fixture here would be
/// answering on a path no real approval ever takes. Callers must have
/// planted a router identity (`plant_router_identity`) in `home`.
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

/// Unattended, an unanswered gate must park immediately — not spend the
/// wait window discovering that nobody is watching. Elapsed time is the
/// assertion: the old path would sit here for the full 300 s timeout.
#[tokio::test]
async fn defer_parks_immediately_instead_of_waiting() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    let t0 = Instant::now();
    let d = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Defer,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-1"),
    )
    .await
    .unwrap();

    assert!(d.deferred && !d.allow, "parked, and never allowed");
    // The bound separates "parked" from "sat out the 300 s timeout"; it is
    // not a latency budget. 5 s tripped on a loaded Windows CI runner
    // (8.04 s of keygen + file I/O under ~8k parallel tests) with the
    // gate having parked correctly, so it is set well clear of that noise
    // while still an order of magnitude under the timeout it guards.
    assert!(
        t0.elapsed() < Duration::from_secs(60),
        "must not wait out the gate timeout: {:?}",
        t0.elapsed()
    );
    assert_eq!(pending_request_ids(tmp.path(), &ch.id).len(), 1);
}

/// The point of the whole feature: an approval given AFTER the run gave up
/// still releases the gate on the next run. The second run mints a fresh
/// `hitl_id`, so this only works because matching is on `action_hash`.
#[tokio::test]
async fn approval_from_an_earlier_run_releases_a_later_one() {
    let tmp = TempDir::new().unwrap();
    let _router = plant_router_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    let first = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Defer,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-1"),
    )
    .await
    .unwrap();
    assert!(first.deferred);

    // A human answers hours later, from any surface.
    let id = pending_request_ids(tmp.path(), &ch.id).pop().unwrap();
    answer(tmp.path(), &ch.id, &id, true);

    let second = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Defer,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-2"),
    )
    .await
    .unwrap();
    assert!(second.allow, "the earlier approval must release this run");
    assert!(!second.deferred);
    assert_eq!(
        second.action_hash, first.action_hash,
        "same action ⇒ same pin"
    );
}

/// A denial is also durable: re-asking every iteration would be nagging,
/// and worse, would let a run eventually catch a distracted "yes".
#[tokio::test]
async fn denial_persists_and_is_not_re_asked() {
    let tmp = TempDir::new().unwrap();
    let _router = plant_router_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Defer,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-1"),
    )
    .await
    .unwrap();
    let id = pending_request_ids(tmp.path(), &ch.id).pop().unwrap();
    answer(tmp.path(), &ch.id, &id, false);

    let d = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Defer,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-2"),
    )
    .await
    .unwrap();
    assert!(!d.allow && !d.deferred, "denied, and not asked again");
    assert_eq!(
        pending_request_ids(tmp.path(), &ch.id).len(),
        1,
        "no second request written"
    );
}

/// A loop re-running the same blocked step must not write one request per
/// iteration; the human should see the question once.
#[tokio::test]
async fn repeated_defers_reuse_the_parked_request() {
    let tmp = TempDir::new().unwrap();
    let _router = plant_router_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    for run in 0..3 {
        let d = gate(
            tmp.path(),
            &ch.id,
            &req(RiskTier::Destructive),
            &GatePolicy {
                yes: false,
                unanswered: Unanswered::Defer,
                auto_approve_tiers: vec![],
            },
            None,
            Some(&format!("run-{run}")),
        )
        .await
        .unwrap();
        assert!(d.deferred);
    }
    assert_eq!(
        pending_request_ids(tmp.path(), &ch.id).len(),
        1,
        "three iterations, one question"
    );
}

/// An approval covers the bytes a human saw. Change the action and the
/// hash changes, so the old approval cannot carry it — the gate asks again
/// rather than executing something nobody agreed to.
#[tokio::test]
async fn an_approval_does_not_carry_to_a_different_action() {
    let tmp = TempDir::new().unwrap();
    // Router-signed approvals, or the one below would not count at all
    // and this test would pass without testing anything.
    let _router = plant_router_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Defer,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-1"),
    )
    .await
    .unwrap();
    let id = pending_request_ids(tmp.path(), &ch.id).pop().unwrap();
    answer(tmp.path(), &ch.id, &id, true);

    let mut other = req(RiskTier::Destructive);
    other.tool_input = serde_json::json!({ "cmd": "rm -rf /" });
    let d = gate(
        tmp.path(),
        &ch.id,
        &other,
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Defer,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-2"),
    )
    .await
    .unwrap();
    assert!(
        d.deferred && !d.allow,
        "a different action must be asked separately"
    );
}

/// `deny` is a policy floor, not a preference: it must refuse even when
/// the channel carries a fresh, valid approval for exactly this action.
/// If an old approval could out-rank the current policy, declaring a fleet
/// off-limits would be advisory only.
#[tokio::test]
async fn deny_mode_outranks_an_existing_approval() {
    let tmp = TempDir::new().unwrap();
    // Router-signed approvals, or the one below would not count at all
    // and this test would pass without testing anything.
    let _router = plant_router_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    // Park a request and approve it under the permissive policy.
    gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Defer,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-1"),
    )
    .await
    .unwrap();
    let id = pending_request_ids(tmp.path(), &ch.id).pop().unwrap();
    answer(tmp.path(), &ch.id, &id, true);

    // Same action, now under `deny`.
    let d = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: false,
            unanswered: Unanswered::Deny,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-2"),
    )
    .await
    .unwrap();
    assert!(!d.allow, "policy floor must outrank a prior approval");
    assert!(!d.deferred, "deny is a verdict, not a parked question");
    assert_eq!(
        pending_request_ids(tmp.path(), &ch.id).len(),
        1,
        "deny must not write a request nobody can answer"
    );
}

/// `deny` also must not out-rank `--yes`… by accident in the other
/// direction: `yes` is unreachable from unattended fleet paths, but if a
/// caller passes both, the floor still wins.
#[tokio::test]
async fn deny_mode_outranks_yes() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    let d = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &GatePolicy {
            yes: true,
            unanswered: Unanswered::Deny,
            auto_approve_tiers: vec![],
        },
        None,
        Some("run-1"),
    )
    .await
    .unwrap();
    assert!(!d.allow, "a floor that --yes can lift is not a floor");
}

/// The TTL bounds how long a decision keeps releasing a gate. Content
/// staleness is the pin's job; this is the clock's half.
#[test]
fn approval_ttl_boundary() {
    let now = chrono::Utc::now();
    assert!(within_approval_ttl(now, now));
    assert!(within_approval_ttl(
        now - chrono::Duration::seconds(mur_common::hitl::APPROVAL_TTL_SECS - 1),
        now
    ));
    assert!(!within_approval_ttl(
        now - chrono::Duration::seconds(mur_common::hitl::APPROVAL_TTL_SECS + 1),
        now
    ));
}

// ── Standing tier grants (P1b) ────────────────────────────────────────

fn write_policy(grant: bool) -> GatePolicy {
    GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: if grant {
            vec![RiskTier::Write]
        } else {
            Vec::new()
        },
    }
}

/// A granted tier runs without a human — but the run must stay answerable:
/// the auto-approval is recorded on the channel as a request+response pair,
/// so "what did this unattended run do without asking me?" has an answer.
#[tokio::test]
async fn granted_write_tier_runs_and_is_audited() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    let mut r = req(RiskTier::Write);
    r.tool_input = serde_json::json!({ "cmd": "touch ./x" });
    let d = gate(
        tmp.path(),
        &ch.id,
        &r,
        &write_policy(true),
        None,
        Some("run-1"),
    )
    .await
    .unwrap();
    assert!(d.allow, "granted tier must not ask: {d:?}");
    assert!(!d.deferred);

    let svc = ChannelService::open(tmp.path()).unwrap();
    let responses: Vec<HitlResponse> = svc
        .load_events(&ch.id)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::HitlResponse)
        .filter_map(|e| serde_json::from_value(e.payload.clone()).ok())
        .collect();
    assert_eq!(
        responses.len(),
        1,
        "the auto-approval must be on the channel"
    );
    assert!(responses[0].allow);
    assert!(
        responses[0].reason.contains("pre-approved"),
        "reason must say WHERE the authority came from: {:?}",
        responses[0].reason
    );
}

/// A human's explicit "no" to THIS action outranks a standing grant for its
/// tier — a decision someone actually made beats a blanket one made in
/// advance.
#[tokio::test]
async fn a_human_denial_outranks_a_tier_grant() {
    let tmp = TempDir::new().unwrap();
    let _router = plant_router_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    // Ask under a no-grant policy, and get denied.
    gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Write),
        &write_policy(false),
        None,
        Some("run-1"),
    )
    .await
    .unwrap();
    let id = pending_request_ids(tmp.path(), &ch.id).pop().unwrap();
    answer(tmp.path(), &ch.id, &id, false);

    // Same action, now under a write grant.
    let d = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Write),
        &write_policy(true),
        None,
        Some("run-2"),
    )
    .await
    .unwrap();
    assert!(!d.allow, "the human said no; the grant must not override");
}

/// The ceiling is enforced inside the gate too, so a hand-edited
/// fleet.yaml that skipped validation still cannot grant `destructive`.
#[tokio::test]
async fn an_ungrantable_tier_is_ignored_even_if_listed() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    let policy = GatePolicy {
        yes: false,
        unanswered: Unanswered::Defer,
        auto_approve_tiers: vec![RiskTier::Destructive],
    };
    let d = gate(
        tmp.path(),
        &ch.id,
        &req(RiskTier::Destructive),
        &policy,
        None,
        Some("run-1"),
    )
    .await
    .unwrap();
    assert!(
        d.deferred && !d.allow,
        "destructive must still wait for a person, not honor the config line"
    );
}
