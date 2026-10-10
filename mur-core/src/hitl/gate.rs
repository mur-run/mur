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
use mur_common::hitl::{
    HitlMode, HitlRequest, HitlResponse, RiskTier, Unanswered, check_fresh, default_mode,
};

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
#[derive(Debug)]
pub struct GateDecision {
    pub allow: bool,
    pub deferred: bool,
    pub reason: String,
    pub action_hash: String,
    /// The id of the parked request, set only when `deferred`. A caller that
    /// defers has to be able to tell a human WHICH request to answer, and
    /// `mur channel approve` matches strictly on this id — not on
    /// `action_hash`. Without it a caller can only print a command the
    /// approve path rejects, which is how a monitor ends up parked with no
    /// reachable way to release it.
    pub hitl_id: Option<String>,
}

/// How often the wait loop re-reads the log, and the default wait budget.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Everything the gate needs to know about the run's approval posture.
///
/// Bundled rather than passed as loose arguments so a future knob cannot be
/// added at a call site without the other two being considered — this is the
/// struct that decides whether an unattended process acts without a human.
#[derive(Debug, Clone, Default)]
pub struct GatePolicy {
    /// `--yes`: auto-approve every Ask-tier action. Interactive/explicit only;
    /// unattended fleet paths pass `false` and must keep doing so.
    pub yes: bool,
    /// What happens when nobody has answered yet.
    pub unanswered: Unanswered,
    /// Tiers the run's owner pre-approved in config. Filtered through
    /// [`mur_common::hitl::tier_may_be_granted`] on the way in AND checked
    /// again here, so a hand-edited fleet.yaml that skipped validation still
    /// cannot grant `destructive`.
    pub auto_approve_tiers: Vec<RiskTier>,
}

impl GatePolicy {
    /// Has this tier been pre-approved for this run?
    fn grants(&self, tier: RiskTier) -> bool {
        mur_common::hitl::tier_may_be_granted(tier) && self.auto_approve_tiers.contains(&tier)
    }
}

/// Gate an action. Read tier returns `allow` immediately.
///
/// `unanswered` selects what happens when an Ask-tier action has no answer
/// yet: `Wait` blocks the caller polling for up to `timeout` (attended),
/// `Defer` parks the request and returns `deferred` at once (unattended), and
/// `Deny` refuses without writing a request at all. `Deny` is a policy floor,
/// so it short-circuits before any lookup — a run declared free of risk-tiered
/// work stays that way even if an older approval for the same action exists.
/// Otherwise a settled decision for the same `action_hash` — from any earlier
/// run, inside the TTL — releases the gate without asking again.
///
/// Takes `mur_home: &Path` rather than `&ChannelService` so `ChannelService` is
/// never held across `.await` — keeping the returned future `Send`.
/// Ordering inside the Ask tier, strictest first: the `Deny` floor, then a
/// settled human decision for this exact action, then a standing tier grant,
/// then park-or-wait. A human's explicit "no" therefore outranks a standing
/// grant — a decision someone actually made about this action beats a blanket
/// one made in advance about its category.
pub async fn gate(
    mur_home: &Path,
    channel_id: &str,
    req: &ActionRequest,
    policy: &GatePolicy,
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
            hitl_id: None,
        }),
        HitlMode::Deny => Ok(GateDecision {
            allow: false,
            deferred: false,
            reason: "policy: deny".into(),
            action_hash: hash,
            hitl_id: None,
        }),
        HitlMode::Ask => {
            // Policy floor: refuse before looking anything up, so an approval
            // recorded under a looser policy cannot release a gate the fleet
            // has since declared off-limits.
            if policy.unanswered == Unanswered::Deny {
                return Ok(GateDecision {
                    allow: false,
                    deferred: false,
                    reason: "policy: approvals disabled for this run".into(),
                    action_hash: hash,
                    hitl_id: None,
                });
            }
            let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
            // Pre-approved by config for this tier. Computed before the scan so
            // it can also skip a stale pending request left over from before
            // the grant was written — but deliberately NOT before the scan's
            // `Settled` arm, so a human's explicit denial still outranks it.
            let granted = policy.grants(req.tier);

            // What does this channel already say about THIS EXACT action?
            // Keyed on `action_hash`, never on `hitl_id`: the id is minted
            // fresh on every call, so an id-keyed lookup can never see the
            // answer a human gave to the previous run — the defect that made
            // late approval impossible and piled up duplicate requests, one
            // per loop iteration, all asking the same question.
            match scan_prior(mur_home, channel_id, &hash)? {
                // Settled (approved or denied) and still inside the TTL:
                // release the gate now. This is what lets an overnight
                // approval be picked up by the next run, and what stops a
                // denial from re-asking every iteration.
                Prior::Settled(d) => return Ok(d),
                // Already parked and unanswered: point at the EXISTING
                // request rather than writing a second one.
                Prior::Pending(existing_id)
                    if !granted && policy.unanswered == Unanswered::Defer =>
                {
                    return Ok(GateDecision {
                        allow: false,
                        deferred: true,
                        reason: format!("awaiting approval ({existing_id})"),
                        action_hash: hash,
                        hitl_id: Some(existing_id),
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
                issued_at: Some(chrono::Utc::now()),
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
            // nobody is watching. A granted tier skips this: it has an answer
            // already, given in advance.
            if !granted && policy.unanswered == Unanswered::Defer {
                return Ok(GateDecision {
                    allow: false,
                    deferred: true,
                    reason: format!("awaiting approval ({hitl_id})"),
                    action_hash: hash,
                    hitl_id: Some(hitl_id),
                });
            }

            let decision = if policy.yes || granted {
                // Record the auto-approval on the channel even though no human
                // saw it: "what did this unattended run do without asking me?"
                // has to be answerable afterwards, and the request+response
                // pair is where that answer lives.
                let why = if policy.yes {
                    "--yes".to_string()
                } else {
                    format!("fleet policy: {:?} tier pre-approved", req.tier).to_lowercase()
                };
                let resp = HitlResponse {
                    hitl_id: hitl_id.clone(),
                    action_hash: hash.clone(),
                    allow: true,
                    reason: why.clone(),
                    surface: if policy.yes { "auto" } else { "policy" }.into(),
                    issued_at: Some(chrono::Utc::now()),
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
                    reason: format!("auto-approved ({why})"),
                    action_hash: hash.clone(),
                    hitl_id: None,
                }
            } else {
                wait_for_response(mur_home, channel_id, &hitl_id, &hash, timeout).await?
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
    /// A human settled this exact action, recently enough to count.
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
/// A response settles the action only if it is the human's
/// (`authority::is_human_authority`), exactly as in the wait loop. The gate's
/// own `--yes` / tier-grant record is router-signed but `System`: it marks its
/// request answered — that request is not pending, and every listing already
/// shows it as answered — yet approves nothing, so a later run asks afresh
/// (#1764). Only a router-signed request is reported as pending. A response outside
/// the TTL leaves the action `None` (ask again), not `Pending` — its request is
/// answered, just too long ago to act on.
fn scan_prior(mur_home: &Path, channel_id: &str, hash: &str) -> Result<Prior> {
    let svc = ChannelService::open(mur_home)?;
    let events = svc.load_events(channel_id)?;
    drop(svc);

    let now = chrono::Utc::now();
    let requests = binding::Requests::collect(mur_home, channel_id, &events);
    let mut responded: std::collections::HashSet<String> = std::collections::HashSet::new();
    // (signed answer time, decision). Newest by signed `issued_at`, never by
    // line order: lines are not signed, so a reordered file must not let an
    // old allow outrank a newer deny (#1764 option C).
    let mut settled: Option<(chrono::DateTime<chrono::Utc>, GateDecision)> = None;
    // (signed request time, id) of the newest answerable request.
    let mut pending: Option<(chrono::DateTime<chrono::Utc>, String)> = None;

    for e in &events {
        match e.kind {
            EventKind::HitlResponse => {
                let Ok(r) = serde_json::from_value::<HitlResponse>(e.payload.clone()) else {
                    continue;
                };
                // Only the router writes answers — see `authority`.
                if !super::authority::is_router_signed(mur_home, channel_id, e) {
                    continue;
                }
                // An answer counts only for the request it names, in time
                // (#1764). An unbound one is ignored outright: it neither
                // settles the action nor marks any request answered.
                let answered_at = match requests.bind(&r, now) {
                    Ok(t) => t,
                    Err(why) => {
                        tracing::warn!(channel_id, hitl_id = %r.hitl_id, ?why, "HitlResponse does not answer its request — ignoring");
                        continue;
                    }
                };
                // Answered — even if it is too old to reuse or is only the
                // gate's own audit record, so the request it answers is not
                // re-reported as still pending.
                responded.insert(r.hitl_id.clone());
                // Only the human's answer decides. Audit records are skipped,
                // not counted as "no": a human's earlier answer still stands.
                if !super::authority::is_human_authority(mur_home, channel_id, e) {
                    continue;
                }
                if r.action_hash != hash || check_fresh(answered_at, now).is_err() {
                    continue;
                }
                // Newest signed decision wins; on a tie a deny wins.
                let newer = settled.as_ref().is_none_or(|(t, d)| {
                    answered_at > *t || (answered_at == *t && d.allow && !r.allow)
                });
                if newer {
                    settled = Some((
                        answered_at,
                        GateDecision {
                            allow: r.allow,
                            deferred: false,
                            reason: if r.allow {
                                format!("approved earlier ({})", r.hitl_id)
                            } else {
                                format!("denied earlier ({})", r.hitl_id)
                            },
                            action_hash: hash.to_string(),
                            hitl_id: None,
                        },
                    ));
                }
            }
            EventKind::HitlRequest => {
                let Ok(q) = serde_json::from_value::<HitlRequest>(e.payload.clone()) else {
                    continue;
                };
                // Only a request an answer could still settle is offered:
                // not re-issued, signed `issued_at`, not expired. Anything
                // else, the gate writes a fresh request instead.
                if q.action_hash != hash
                    || !super::authority::is_router_signed(mur_home, channel_id, e)
                    || !requests.is_answerable(&q.hitl_id, now)
                {
                    continue;
                }
                let Some(asked) = q.issued_at else { continue };
                if pending.as_ref().is_none_or(|(t, _)| asked >= *t) {
                    pending = Some((asked, q.hitl_id));
                }
            }
            _ => {}
        }
    }

    if let Some((_, d)) = settled {
        return Ok(Prior::Settled(d));
    }
    match pending {
        Some((_, id)) if !responded.contains(&id) => Ok(Prior::Pending(id)),
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
    timeout: Duration,
) -> Result<GateDecision> {
    // No `MUR_CHANNEL_REQUIRE_SIG` input, on purpose: an approval must be
    // router-signed whatever that variable says. Its default-off tolerance of
    // unsigned events is migration safety for ordinary history, and applied
    // here it let any agent approve its own gated action.
    let start = Instant::now();
    loop {
        // Open, read, drop — then await the sleep. A response releases the
        // gate only if the router signed it as the human (see
        // `authority::is_human_authority`). An agent's own correctly-signed
        // reply is a verified statement by that agent, not an approval; it is
        // filtered out and the loop keeps waiting.
        let (found, requests) = {
            let svc = ChannelService::open(mur_home)?;
            let evs = svc.load_events(channel_id)?;
            drop(svc);
            let requests = binding::Requests::collect(mur_home, channel_id, &evs);
            let found = evs.into_iter().rev().find(|e| {
                if e.kind != EventKind::HitlResponse
                    || e.payload.get("hitl_id").and_then(|v| v.as_str()) != Some(hitl_id)
                {
                    return false;
                }
                if !super::authority::is_human_authority(mur_home, channel_id, e) {
                    tracing::warn!(
                        channel_id,
                        hitl_id,
                        actor = ?e.actor,
                        "HitlResponse is not signed by MUR's router as the human — ignoring"
                    );
                    return false;
                }
                true
            });
            (found, requests)
        };
        let deny = |reason: String| GateDecision {
            allow: false,
            deferred: false,
            reason,
            action_hash: expected_hash.to_string(),
            hitl_id: None,
        };
        // Same binding rule as `scan_prior`. Our own request was signed a
        // second time under this id: no answer to it can be trusted, so fail
        // closed rather than wait.
        if requests.is_reissued(hitl_id) {
            return Ok(deny(
                "hitl_reissued: request id signed more than once".into(),
            ));
        }
        if let Some(resp) = found {
            let Ok(r) = serde_json::from_value::<HitlResponse>(resp.payload) else {
                return Ok(deny(
                    "hitl_malformed: response payload does not parse".into(),
                ));
            };
            if r.action_hash != expected_hash {
                return Ok(deny("hitl_drift: response action_hash mismatch".into()));
            }
            match requests.bind(&r, chrono::Utc::now()) {
                Ok(_) => {
                    return Ok(GateDecision {
                        allow: r.allow,
                        deferred: false,
                        reason: if r.allow {
                            "approved".into()
                        } else {
                            "denied".into()
                        },
                        action_hash: expected_hash.to_string(),
                        hitl_id: None,
                    });
                }
                // No router-signed request under this id parses — the gate
                // writes its own before waiting, so this is not an answer to
                // anything it asked. Keep waiting; the timeout fails closed.
                Err(binding::Unbound::NoRequest) => {}
                Err(binding::Unbound::HashMismatch) => {
                    return Ok(deny("hitl_drift: response action_hash mismatch".into()));
                }
                Err(binding::Unbound::Reissued) => {
                    return Ok(deny(
                        "hitl_reissued: request id signed more than once".into(),
                    ));
                }
                // Fail closed and say why: e.g. a phone whose clock is days
                // off, or an answer from a writer older than signed time.
                Err(why @ (binding::Unbound::Legacy | binding::Unbound::Time(_))) => {
                    return Ok(deny(format!("hitl_untimely: {why:?}")));
                }
            }
        }
        if start.elapsed() >= timeout {
            return Ok(GateDecision {
                allow: false,
                deferred: false,
                reason: "hitl timeout".into(),
                action_hash: expected_hash.to_string(),
                hitl_id: None,
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

mod binding;

#[cfg(test)]
mod human_authority_tests;
#[cfg(test)]
mod response_binding_tests;
#[cfg(test)]
mod tests;
