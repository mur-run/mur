//! Risk-tiered, hash-pinned approval gate over a Channel (v3c). Writes a durable
//! HitlRequest, waits for a HitlResponse (CLI `mur channel approve`, or a future
//! Hub/iOS UI), and returns the decision. The channel pair is a MIRROR — single
//! trusted writer per the v3 trust-model invariant; per-event signing (authority
//! for headless approval) is v3d.
//!
//! Design note: `gate()` takes `mur_home: &Path` (not `&ChannelService`) so that
//! `ChannelService` (which wraps a `RefCell<Connection>` and is therefore `!Sync`)
//! is opened and dropped within each synchronous section, never held across an
//! `.await` point. This keeps the future `Send` so it can run inside
//! `tokio::task::spawn`.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelState, EventKind};
use mur_common::hitl::{HitlMode, HitlRequest, HitlResponse, RiskTier, default_mode};

use crate::channel_writer::ROUTER_AGENT;
use crate::hitl::pin::action_hash;

/// What the caller wants to do. `tool_input` must be POST-substitution.
pub struct ActionRequest {
    pub tier: RiskTier,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub step_or_call_id: String,
    pub agent_id: String,
    pub summary: String,
}

/// The gate's verdict. `action_hash` is the pin the caller MUST re-verify just
/// before executing (fail-closed on mismatch). `deferred: true` (which implies
/// `allow: false`) means nobody answered AND nobody was made to wait: the
/// request is parked durably in the channel and the caller should mark the
/// step blocked — not failed — so a later approval can release it.
pub struct GateDecision {
    pub allow: bool,
    pub deferred: bool,
    pub reason: String,
    pub action_hash: String,
}

/// How often the wait loop re-reads the log, and the default wait budget.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Approvals and denials settle a gate for this long. Content staleness is
/// already handled by the hash pin (any input change = a different hash); the
/// TTL bounds TIME staleness, so a weeks-old approval cannot release a gate
/// nobody remembers granting.
pub(crate) const HITL_APPROVAL_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Pure TTL predicate — split out so the boundary is testable without
/// backdating channel events.
pub(crate) fn within_approval_ttl(
    event_ts: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    (now - event_ts).num_seconds() <= HITL_APPROVAL_TTL_SECS
}

/// Gate an action. `yes` auto-approves Ask-tier actions (records an `auto`
/// HitlResponse for the audit trail). Read tier returns `allow` immediately.
///
/// `defer` selects what happens when an Ask-tier action has no answer yet:
/// `false` blocks the caller polling for up to `timeout` (attended);
/// `true` parks the request and returns `deferred` at once (unattended).
/// Either way a settled decision for the same `action_hash` — from any earlier
/// run, inside the TTL — releases the gate without asking again.
///
/// Takes `mur_home: &Path` rather than `&ChannelService` so `ChannelService` is
/// never held across `.await` — keeping the returned future `Send`.
pub async fn gate(
    mur_home: &Path,
    channel_id: &str,
    req: &ActionRequest,
    yes: bool,
    defer: bool,
    timeout: Option<Duration>,
    run_id: Option<&str>,
) -> Result<GateDecision> {
    let hash = action_hash(
        &req.tool_name,
        &req.tool_input,
        channel_id,
        &req.step_or_call_id,
        &req.agent_id,
    );

    match default_mode(req.tier) {
        HitlMode::Auto => Ok(GateDecision {
            allow: true,
            deferred: false,
            reason: "read-tier: auto".into(),
            action_hash: hash,
        }),
        HitlMode::Deny => Ok(GateDecision {
            allow: false,
            deferred: false,
            reason: "policy: deny".into(),
            action_hash: hash,
        }),
        HitlMode::Ask => {
            let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
            // Resolve signature-enforcement ONCE here (not deep in the poll
            // loop) so `wait_for_response` is pure w.r.t. config and tests never
            // race on a process-global env var. Only explicit truthy values
            // enable enforcement: `=0` / `=false` must NOT turn it on.
            let require_sig = crate::channel_verify::require_sig_from_env();

            // What does this channel already say about THIS EXACT action?
            // Keyed on `action_hash`, never on `hitl_id`: the id is minted
            // fresh on every call, so an id-keyed lookup can never see the
            // answer a human gave to the previous run — the defect that made
            // late approval impossible and piled up duplicate requests, one
            // per loop iteration, all asking the same question.
            match scan_prior(mur_home, channel_id, &hash, require_sig)? {
                // Settled (approved or denied) and still inside the TTL:
                // release the gate now. This is what lets an overnight
                // approval be picked up by the next run, and what stops a
                // denial from re-asking every iteration.
                Prior::Settled(d) => return Ok(d),
                // Already parked and unanswered: point at the EXISTING
                // request rather than writing a second one.
                Prior::Pending(existing_id) if defer => {
                    return Ok(GateDecision {
                        allow: false,
                        deferred: true,
                        reason: format!("awaiting approval ({existing_id})"),
                        action_hash: hash,
                    });
                }
                _ => {}
            }

            let hitl_id = format!("hitl-{}", uuid::Uuid::now_v7());
            let request = HitlRequest {
                hitl_id: hitl_id.clone(),
                action_hash: hash.clone(),
                tier: req.tier,
                tool_name: req.tool_name.clone(),
                tool_input: req.tool_input.clone(),
                step_or_call_id: req.step_or_call_id.clone(),
                agent_id: req.agent_id.clone(),
                timeout_ms: timeout.as_millis() as u64,
                summary: req.summary.clone(),
            };
            // Open, write, drop — never cross an await with the service open.
            // The router ("mur") signs the events it writes (v3d) so a reader
            // can verify authority; falls back to unsigned when no identity.
            {
                let svc = ChannelService::open(mur_home)?;
                crate::channel_writer::append_as_writer(
                    &svc,
                    mur_home,
                    channel_id,
                    ROUTER_AGENT,
                    ChannelActor::System,
                    EventKind::HitlRequest,
                    serde_json::to_value(&request)?,
                    None,
                )?;
                // Attributed to the run that paused, like every other executor
                // event: a rebuild filters BY `run_id`, so an unstamped
                // transition belongs to no run and is invisible to every
                // rebuild — a run that loses its cache while waiting on this
                // gate would come back as `Working`, disagreeing with the
                // channel about the one state the operator needs to see.
                svc.transition(
                    channel_id,
                    ChannelState::InputRequired,
                    ChannelActor::System,
                    run_id,
                )?;
            }

            // Park instead of polling. The request is durable and the channel
            // stays `InputRequired`, so the answer can arrive at any time from
            // any surface — nobody is made to wait 5 minutes to learn that
            // nobody is watching.
            if defer {
                return Ok(GateDecision {
                    allow: false,
                    deferred: true,
                    reason: format!("awaiting approval ({hitl_id})"),
                    action_hash: hash,
                });
            }

            let decision = if yes {
                let resp = HitlResponse {
                    hitl_id: hitl_id.clone(),
                    action_hash: hash.clone(),
                    allow: true,
                    reason: "--yes".into(),
                    surface: "auto".into(),
                };
                {
                    let svc = ChannelService::open(mur_home)?;
                    crate::channel_writer::append_as_writer(
                        &svc,
                        mur_home,
                        channel_id,
                        ROUTER_AGENT,
                        ChannelActor::System,
                        EventKind::HitlResponse,
                        serde_json::to_value(&resp)?,
                        None,
                    )?;
                }
                GateDecision {
                    allow: true,
                    deferred: false,
                    reason: "auto-approved (--yes)".into(),
                    action_hash: hash.clone(),
                }
            } else {
                wait_for_response(mur_home, channel_id, &hitl_id, &hash, require_sig, timeout)
                    .await?
            };

            {
                let svc = ChannelService::open(mur_home)?;
                svc.transition(
                    channel_id,
                    ChannelState::Working,
                    ChannelActor::System,
                    run_id,
                )?;
            }
            Ok(decision)
        }
    }
}

/// What the channel already knows about one specific action.
enum Prior {
    /// A human (or `--yes`) settled this exact action, recently enough to count.
    Settled(GateDecision),
    /// A request for this exact action is parked and unanswered.
    Pending(String),
    /// Never asked — or asked and answered too long ago to still count.
    None,
}

/// Look up prior HITL traffic for `hash` in one pass over the channel log.
///
/// Matching is on `action_hash`, the deterministic function of
/// (tool, input, channel, step, agent) — NOT on `hitl_id`, which is minted
/// per call and therefore cannot connect a run to the answer given to an
/// earlier one. Two consequences fall out of that choice, both wanted:
/// re-running a workflow picks up an approval granted overnight, and changing
/// the action's input changes the hash, so no approval is ever replayed
/// against bytes a human did not see.
///
/// Responses are verified per-actor (v3d-2) before they count; an unverifiable
/// response is ignored exactly as the wait loop ignores it. A response outside
/// the TTL leaves the action `None` (ask again), not `Pending` — its request is
/// answered, just too long ago to act on.
fn scan_prior(mur_home: &Path, channel_id: &str, hash: &str, require_sig: bool) -> Result<Prior> {
    let svc = ChannelService::open(mur_home)?;
    let events = svc.load_events(channel_id)?;
    drop(svc);

    let now = chrono::Utc::now();
    let mut responded: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut settled: Option<GateDecision> = None;
    let mut pending_id: Option<String> = None;

    for e in &events {
        match e.kind {
            EventKind::HitlResponse => {
                let Ok(r) = serde_json::from_value::<HitlResponse>(e.payload.clone()) else {
                    continue;
                };
                if !crate::channel_verify::verify_event(mur_home, channel_id, e, require_sig) {
                    continue;
                }
                // Answered — even if it later fails the TTL check, so the
                // request it answers is not re-reported as still pending.
                responded.insert(r.hitl_id.clone());
                if r.action_hash == hash && within_approval_ttl(e.ts, now) {
                    // Later events overwrite earlier ones: the newest decision
                    // for an action is the one that counts.
                    settled = Some(GateDecision {
                        allow: r.allow,
                        deferred: false,
                        reason: if r.allow {
                            format!("approved earlier ({})", r.hitl_id)
                        } else {
                            format!("denied earlier ({})", r.hitl_id)
                        },
                        action_hash: hash.to_string(),
                    });
                }
            }
            EventKind::HitlRequest => {
                let Ok(q) = serde_json::from_value::<HitlRequest>(e.payload.clone()) else {
                    continue;
                };
                if q.action_hash == hash
                    && crate::channel_verify::verify_event(mur_home, channel_id, e, require_sig)
                {
                    pending_id = Some(q.hitl_id);
                }
            }
            _ => {}
        }
    }

    if let Some(d) = settled {
        return Ok(Prior::Settled(d));
    }
    match pending_id {
        Some(id) if !responded.contains(&id) => Ok(Prior::Pending(id)),
        _ => Ok(Prior::None),
    }
}

/// Poll the log for a HitlResponse matching `hitl_id`. Opens the service fresh
/// on each poll so we never hold `ChannelService` across an `.await` point.
/// On drift or timeout, deny (fail-closed).
async fn wait_for_response(
    mur_home: &Path,
    channel_id: &str,
    hitl_id: &str,
    expected_hash: &str,
    require: bool,
    timeout: Duration,
) -> Result<GateDecision> {
    // `require` (resolved once by the caller from `MUR_CHANNEL_REQUIRE_SIG`):
    // when true, an unsigned (or absent-pubkey) HitlResponse is NOT trusted
    // (fail-closed). When false, an unsigned response is still accepted so
    // pre-v3d channels keep working (migration-safe). This fn reads no global
    // config itself, so it is race-free under multi-threaded `cargo test`.
    let start = Instant::now();
    loop {
        // Open, read, drop — then await the sleep. Verify each candidate per
        // its OWN actor's key (v3d-2): `crate::channel_verify::verify_event`
        // resolves the signing pubkey from the event's actor (Agent{id} → that
        // agent's home; System/Human → the router), so a delegated specialist's
        // self-signed reply verifies against ITS key — not a single writer's.
        // A forged/unsigned-when-required HitlResponse fails verification, is
        // filtered out, and can never release the gate — the loop keeps waiting.
        let found = {
            let svc = ChannelService::open(mur_home)?;
            let evs = svc.load_events(channel_id)?;
            evs.into_iter().rev().find(|e| {
                if e.kind != EventKind::HitlResponse
                    || e.payload.get("hitl_id").and_then(|v| v.as_str()) != Some(hitl_id)
                {
                    return false;
                }
                if !crate::channel_verify::verify_event(mur_home, channel_id, e, require) {
                    tracing::warn!(
                        channel_id,
                        hitl_id,
                        "HitlResponse failed per-actor signature verification — ignoring"
                    );
                    return false;
                }
                true
            })
        };
        if let Some(resp) = found {
            let echoed = resp
                .payload
                .get("action_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if echoed != expected_hash {
                return Ok(GateDecision {
                    allow: false,
                    deferred: false,
                    reason: "hitl_drift: response action_hash mismatch".into(),
                    action_hash: expected_hash.to_string(),
                });
            }
            let allow = resp
                .payload
                .get("allow")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            return Ok(GateDecision {
                allow,
                deferred: false,
                reason: if allow {
                    "approved".into()
                } else {
                    "denied".into()
                },
                action_hash: expected_hash.to_string(),
            });
        }
        if start.elapsed() >= timeout {
            return Ok(GateDecision {
                allow: false,
                deferred: false,
                reason: "hitl timeout".into(),
                action_hash: expected_hash.to_string(),
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
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
            false,
            false,
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
            true,
            false,
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
        let d = gate(tmp.path(), &ch.id, &r, true, false, None, Some("run-g"))
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
        svc.append(
            &ch.id,
            ChannelActor::System,
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
            false,
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
    fn plant_router_identity(home: &Path) -> AgentIdentity {
        let agent_home = home.join("agents").join(ROUTER_AGENT);
        std::fs::create_dir_all(&agent_home).unwrap();
        let id = AgentIdentity::generate();
        id.save(&agent_home).unwrap();
        id
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
            ChannelActor::System,
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
            false,
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
            ChannelActor::System,
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
            false,
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

    /// With signature enforcement on (`require = true`), an UNSIGNED response is
    /// ignored (fail-closed) even though `allow:true` — it cannot release the
    /// gate. Passing `require` as a parameter keeps this test free of any
    /// process-global env mutation, so it never races sibling tests.
    #[tokio::test]
    async fn unsigned_when_required_does_not_release() {
        let tmp = TempDir::new().unwrap();
        let _router = plant_router_identity(tmp.path());
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        let resp = resp_with_hash("h-unsigned", "EXPECTED");
        svc.append(
            &ch.id,
            ChannelActor::System,
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
            true,
            std::time::Duration::from_millis(900),
        )
        .await
        .unwrap();
        assert!(
            !d.allow,
            "unsigned response must not release when signatures are required"
        );
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
        svc.append(
            ch,
            ChannelActor::System,
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
            false,
            true,
            None,
            Some("run-1"),
        )
        .await
        .unwrap();

        assert!(d.deferred && !d.allow, "parked, and never allowed");
        assert!(
            t0.elapsed() < Duration::from_secs(5),
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
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        drop(svc);

        let first = gate(
            tmp.path(),
            &ch.id,
            &req(RiskTier::Destructive),
            false,
            true,
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
            false,
            true,
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
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        drop(svc);

        gate(
            tmp.path(),
            &ch.id,
            &req(RiskTier::Destructive),
            false,
            true,
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
            false,
            true,
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
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        drop(svc);

        for run in 0..3 {
            let d = gate(
                tmp.path(),
                &ch.id,
                &req(RiskTier::Destructive),
                false,
                true,
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
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        drop(svc);

        gate(
            tmp.path(),
            &ch.id,
            &req(RiskTier::Destructive),
            false,
            true,
            None,
            Some("run-1"),
        )
        .await
        .unwrap();
        let id = pending_request_ids(tmp.path(), &ch.id).pop().unwrap();
        answer(tmp.path(), &ch.id, &id, true);

        let mut other = req(RiskTier::Destructive);
        other.tool_input = serde_json::json!({ "cmd": "rm -rf /" });
        let d = gate(tmp.path(), &ch.id, &other, false, true, None, Some("run-2"))
            .await
            .unwrap();
        assert!(
            d.deferred && !d.allow,
            "a different action must be asked separately"
        );
    }

    /// The TTL bounds how long a decision keeps releasing a gate. Content
    /// staleness is the pin's job; this is the clock's half.
    #[test]
    fn approval_ttl_boundary() {
        let now = chrono::Utc::now();
        assert!(within_approval_ttl(now, now));
        assert!(within_approval_ttl(
            now - chrono::Duration::seconds(HITL_APPROVAL_TTL_SECS - 1),
            now
        ));
        assert!(!within_approval_ttl(
            now - chrono::Duration::seconds(HITL_APPROVAL_TTL_SECS + 1),
            now
        ));
    }
}
