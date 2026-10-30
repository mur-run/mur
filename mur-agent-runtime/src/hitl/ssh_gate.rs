//! Gate B for ssh-agent sign requests (plan Task 3.2): the production
//! [`SignGate`] the proxy asks after its policy passes.
//!
//! Reuses gate B's parts unchanged: the remembered-decision store, the
//! `hitl_id → oneshot` map `tool/hitl_respond` answers, and `Parking`
//! (what `hitl/pending` lists, so `mur agent hitl <agent> approve` works
//! from any terminal). A sign request belongs to no turn, so the prompt goes
//! out on the supervisor's broadcast notifier with an empty `task_id`.
//!
//! Unattended (spec "HITL wiring"): defer, never time out. The proxy waits
//! up to `wait` so an attached person can answer while `ssh` is still
//! connected; after that the sign is refused `hitl_deferred` but the ask
//! stays pending, and its answer, whenever it comes, is recorded under
//! `action_hash`. The next sign request for the same pair is then settled
//! without a prompt.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use super::parked::{ParkedCall, Parking};
use super::ssh::{SSH_GATE_STEP, SSH_SIGN_TOOL, ssh_action_hash};
use super::store::{DecisionStore, Settled};
use super::{HitlApprovals, HitlDecision};
use crate::ssh_proxy::gate::{GateOutcome, SignAsk, SignGate};

/// The prompt line for an audit-only agent, verbatim from the spec ("What
/// audit-only does and does not change").
pub const AUDIT_ONLY_LINE: &str =
    "this agent can reach your ssh-agent directly; this approval is not the only gate";

/// The tier the prompt carries. Above `tier_may_be_granted`, so no session
/// lane, `/auto`, or config grant can answer it: every first signature for
/// a pair is a person's decision, and remembering is the only way to be
/// asked less (spec "no HITL switches").
pub const SSH_SIGN_RISK: mur_common::hitl::RiskTier = mur_common::hitl::RiskTier::Privileged;

/// The `task_id` of a prompt that belongs to no turn.
const NO_TASK: &str = "";

/// How long a pending ask is listed in `hitl/pending` after the proxy stops
/// waiting. Display only; the ask itself never times out.
const PENDING_LISTED_FOR: Duration =
    Duration::from_secs(mur_common::hitl::APPROVAL_TTL_SECS as u64);

/// One ask in flight; `None` until answered.
type Answer = watch::Receiver<Option<bool>>;

pub struct SshSignGate {
    pub agent: String,
    pub store: Arc<dyn DecisionStore>,
    pub approvals: HitlApprovals,
    pub notifier: mpsc::Sender<serde_json::Value>,
    pub parking: Parking,
    /// How long one sign request waits for an answer before it is deferred.
    pub wait: Duration,
    /// The proxy is audit-only for this agent: the prompt says so.
    pub audit_only: bool,
    /// `action_hash` → the ask already out for it, so concurrent sign
    /// requests for one pair raise one prompt.
    pub pending: Arc<Mutex<HashMap<String, Answer>>>,
}

#[async_trait::async_trait]
impl SignGate for SshSignGate {
    async fn decide(&self, ask: &SignAsk) -> GateOutcome {
        let hash = ssh_action_hash(&self.agent, &ask.key_fp, &ask.host_key_fp);
        match self.store.lookup(&hash).await {
            Some(Settled::Allow) => return GateOutcome::Allow,
            Some(Settled::Deny) => return GateOutcome::Deny,
            None => {}
        }
        let mut answer = match self.join_or_raise(&hash, ask).await {
            Ok(a) => a,
            Err(why) => return GateOutcome::Unavailable(why),
        };
        let settled = tokio::time::timeout(self.wait, answer.wait_for(Option::is_some)).await;
        match settled {
            Ok(Ok(v)) => match *v {
                Some(true) => GateOutcome::Allow,
                _ => GateOutcome::Deny,
            },
            // The waiter went away without an answer: the approvals map was
            // dropped (runtime shutting down).
            Ok(Err(_)) => GateOutcome::Unavailable("the approval was dropped unanswered".into()),
            Err(_) => GateOutcome::Deferred,
        }
    }
}

impl SshSignGate {
    /// The answer channel of the ask already out for `hash`, or a new ask.
    ///
    /// The pending entry is reserved under one lock, before any await, so
    /// concurrent requests for a pair can never raise two prompts. Only the
    /// reservation's owner publishes the prompt and removes the entry.
    async fn join_or_raise(&self, hash: &str, ask: &SignAsk) -> Result<Answer, String> {
        let (done_tx, done_rx) = {
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(a) = pending.get(hash) {
                return Ok(a.clone());
            }
            let (tx, rx) = watch::channel(None);
            pending.insert(hash.to_string(), rx.clone());
            (tx, rx)
        };
        let hitl_id = uuid::Uuid::now_v7().to_string();
        let (tx, rx) = oneshot::channel::<HitlDecision>();
        self.approvals.lock().await.insert(hitl_id.clone(), tx);
        let label = label(ask);
        let now = chrono::Utc::now();
        self.parking.park(ParkedCall {
            hitl_id: hitl_id.clone(),
            task_id: NO_TASK.into(),
            tool_name: SSH_SIGN_TOOL.into(),
            label: label.clone(),
            parked_at: now,
            expires_at: now
                + chrono::Duration::from_std(PENDING_LISTED_FOR).unwrap_or(chrono::Duration::MAX),
        });
        if self
            .notifier
            .send(self.notification(&hitl_id, hash, ask))
            .await
            .is_err()
        {
            self.approvals.lock().await.remove(&hitl_id);
            self.parking.unpark(&hitl_id);
            release(&self.pending, hash, &done_rx);
            // Dropping `done_tx` refuses every request that joined.
            drop(done_tx);
            return Err("the approval channel is closed".into());
        }
        let store = self.store.clone();
        let parking = self.parking.clone();
        let hash = hash.to_string();
        let pending = self.pending.clone();
        let mine = done_rx.clone();
        // Outlives the sign request: a late answer is still recorded.
        tokio::spawn(async move {
            let decision = rx.await;
            parking.unpark(&hitl_id);
            if let Ok(d) = &decision {
                store
                    .record(mur_common::hitl::HitlResponse {
                        hitl_id: hitl_id.clone(),
                        action_hash: hash.clone(),
                        allow: d.allow,
                        reason: d.reason.clone().unwrap_or_default(),
                        surface: d.surface.clone().unwrap_or_else(|| "unknown".into()),
                        issued_at: Some(chrono::Utc::now()),
                        label: Some(label),
                    })
                    .await;
            }
            release(&pending, &hash, &mine);
            let _ = done_tx.send(decision.ok().map(|d| d.allow));
        });
        Ok(done_rx)
    }

    fn notification(&self, hitl_id: &str, hash: &str, ask: &SignAsk) -> serde_json::Value {
        let input = serde_json::json!({
            "key_fp": ask.key_fp,
            "key_label": ask.key_label,
            "host_key_fp": ask.host_key_fp,
            "host_label": ask.host_label,
        });
        let call = serde_json::json!({
            "hitl_id": hitl_id,
            "step_id": SSH_GATE_STEP,
            "tool_name": SSH_SIGN_TOOL,
            "tool_input": input,
            "action_hash": hash,
            "risk": SSH_SIGN_RISK,
            "prompt": self.prompt(ask),
        });
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool/approval_needed",
            "params": {
                "batch_id": uuid::Uuid::now_v7().to_string(),
                "task_id": NO_TASK,
                "timeout_ms": self.wait.as_millis() as u64,
                "step_id": call["step_id"],
                "hitl_id": call["hitl_id"],
                "tool_name": call["tool_name"],
                "tool_input": call["tool_input"],
                "risk": call["risk"],
                "prompt": call["prompt"],
                "calls": [call],
            }
        })
    }

    fn prompt(&self, ask: &SignAsk) -> String {
        let mut p = format!(
            "Agent {} wants to sign an SSH login with key {} for host {}.",
            self.agent,
            named(ask.key_label.as_deref(), &ask.key_fp),
            named(ask.host_label.as_deref(), &ask.host_key_fp),
        );
        if self.audit_only {
            p.push('\n');
            p.push_str(AUDIT_ONLY_LINE);
        }
        p
    }
}

/// Remove `hash`'s pending entry only if it is still `mine`, so an owner
/// never clears an ask it did not raise.
fn release(pending: &Mutex<HashMap<String, Answer>>, hash: &str, mine: &Answer) {
    let mut pending = pending.lock().unwrap_or_else(|p| p.into_inner());
    if pending.get(hash).is_some_and(|a| a.same_channel(mine)) {
        pending.remove(hash);
    }
}

/// `label (fp)`, or the fingerprint alone.
fn named(label: Option<&str>, fp: &str) -> String {
    match label {
        Some(l) => format!("{l} ({fp})"),
        None => fp.to_string(),
    }
}

/// The decision-log label: names and fingerprints only, never session data.
fn label(ask: &SignAsk) -> String {
    format!(
        "{SSH_SIGN_TOOL}: {} -> {}",
        ask.key_label.as_deref().unwrap_or(&ask.key_fp),
        ask.host_label.as_deref().unwrap_or(&ask.host_key_fp),
    )
}

#[cfg(test)]
#[path = "ssh_gate_tests.rs"]
mod tests;
