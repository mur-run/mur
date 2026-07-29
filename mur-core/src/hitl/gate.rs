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
/// before executing (fail-closed on mismatch).
pub struct GateDecision {
    pub allow: bool,
    pub reason: String,
    pub action_hash: String,
}

/// How often the wait loop re-reads the log, and the default wait budget.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Gate an action. `yes` auto-approves Ask-tier actions (records an `auto`
/// HitlResponse for the audit trail). Read tier returns `allow` immediately.
///
/// Takes `mur_home: &Path` rather than `&ChannelService` so `ChannelService` is
/// never held across `.await` — keeping the returned future `Send`.
pub async fn gate(
    mur_home: &Path,
    channel_id: &str,
    req: &ActionRequest,
    yes: bool,
    timeout: Option<Duration>,
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
            reason: "read-tier: auto".into(),
            action_hash: hash,
        }),
        HitlMode::Deny => Ok(GateDecision {
            allow: false,
            reason: "policy: deny".into(),
            action_hash: hash,
        }),
        HitlMode::Ask => {
            let hitl_id = format!("hitl-{}", uuid::Uuid::now_v7());
            let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
            // Resolve signature-enforcement ONCE here (not deep in the poll
            // loop) so `wait_for_response` is pure w.r.t. config and tests never
            // race on a process-global env var. Only explicit truthy values
            // enable enforcement: `=0` / `=false` must NOT turn it on.
            let require_sig = crate::channel_verify::require_sig_from_env();
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
                svc.transition(
                    channel_id,
                    ChannelState::InputRequired,
                    ChannelActor::System,
                )?;
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
                    reason: "auto-approved (--yes)".into(),
                    action_hash: hash.clone(),
                }
            } else {
                wait_for_response(mur_home, channel_id, &hitl_id, &hash, require_sig, timeout)
                    .await?
            };

            {
                let svc = ChannelService::open(mur_home)?;
                svc.transition(channel_id, ChannelState::Working, ChannelActor::System)?;
            }
            Ok(decision)
        }
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
        let d = gate(tmp.path(), &ch.id, &req(RiskTier::Read), false, None)
            .await
            .unwrap();
        assert!(d.allow);
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
        let d = gate(tmp.path(), &ch.id, &r, true, None).await.unwrap();
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
}
