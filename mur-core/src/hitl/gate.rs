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
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

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
            {
                let svc = ChannelService::open(mur_home)?;
                svc.append(
                    channel_id,
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
                    svc.append(
                        channel_id,
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
                wait_for_response(mur_home, channel_id, &hitl_id, &hash, timeout).await?
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
    timeout: Duration,
) -> Result<GateDecision> {
    let start = Instant::now();
    loop {
        // Open, read, drop — then await the sleep.
        let found = {
            let svc = ChannelService::open(mur_home)?;
            let evs = svc.load_events(channel_id)?;
            evs.into_iter().rev().find(|e| {
                e.kind == EventKind::HitlResponse
                    && e.payload.get("hitl_id").and_then(|v| v.as_str()) == Some(hitl_id)
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
            std::time::Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(!d.allow, "mismatched action_hash must fail-closed");
        assert!(d.reason.contains("drift"));
        let _ = r;
    }
}
