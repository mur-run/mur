//! Gate B, batched: every `Ask` call of one LLM response is resolved before
//! any of them runs â a settled decision from the store, or ONE
//! `tool/approval_needed` carrying all of them. Each call keeps its own
//! `hitl_id` and its own oneshot, so `tool/hitl_respond` is unchanged and no
//! single answer can release more than one call.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::store::{DecisionStore, Settled, chat_action_hash};
use super::{HitlApprovals, HitlDecision};

/// One `Ask` call awaiting a decision.
#[derive(Clone)]
pub struct PendingCall {
    pub call_id: String,
    pub step_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub action_hash: String,
    /// #1600: the tier the CLI's session lanes gate on. Gate B fills it with
    /// `approval_risk(..)` (the profile's strictest declared `risk:`) maxed
    /// with any layer-3 intent raise, so it is the FINAL tier, not the
    /// profile-declared one. `None` = undeclared and not raised.
    pub risk: Option<mur_common::hitl::RiskTier>,
    /// #2175: this call alone neither reads nor writes `store`, whatever the
    /// gate's `remember` says — a `pre_tool_use` hook asked a question about
    /// this turn (B0 rule 4), not about the tool.
    pub one_shot: bool,
}

pub struct BatchGate<'a> {
    pub task_id: &'a str,
    pub timeout: Duration,
    pub approvals: &'a HitlApprovals,
    pub notifier: &'a tokio::sync::mpsc::Sender<serde_json::Value>,
    pub store: Option<&'a Arc<dyn DecisionStore>>,
    /// Records which task raised each approval, so a trusted shim can answer
    /// only its own turn's. `None` = no CLI-spawn track wired.
    pub shim_trust: Option<&'a crate::hitl::shim_ticket::ShimTrust>,
    /// `false` = one-shot approvals for this turn: `store` is neither read nor
    /// written (#1783). A review session's answer must not become the agent's
    /// standing policy, and a standing policy must not answer for the review.
    pub remember: bool,
    /// #2085: list each asked call in `hitl/pending` while it waits, for a
    /// headless turn whose answer comes from another terminal. `None` = an
    /// attached client is the audience; nothing to list.
    pub parking: Option<&'a super::parked::Parking>,
}

const TIMED_OUT: &str = "timed out";

/// True for the decision [`BatchGate::ask`] gives a call nobody answered in
/// time â no human said anything, so a headless caller reports it as still
/// needing approval rather than as a denial (#2085).
pub fn is_timeout(d: &HitlDecision) -> bool {
    !d.allow && d.reason.as_deref() == Some(TIMED_OUT)
}

impl BatchGate<'_> {
    /// Resolve every pending call. Returns `call_id â decision`; every input
    /// call has an entry (timeout and store-denials included).
    pub async fn resolve(&self, calls: Vec<PendingCall>) -> HashMap<String, HitlDecision> {
        let (mut out, ask) = self.recall(calls).await;
        out.extend(self.ask(ask).await);
        out
    }

    /// Split `calls` into those a remembered decision settles (answered here)
    /// and those only a human can answer (returned for [`Self::ask`]). Two
    /// steps so a caller can tell a call that stopped for a human from one
    /// the store answered â #1814 counts only the first.
    pub async fn recall(
        &self,
        calls: Vec<PendingCall>,
    ) -> (HashMap<String, HitlDecision>, Vec<PendingCall>) {
        let mut out = HashMap::with_capacity(calls.len());
        let mut ask = Vec::new();
        for c in calls {
            let settled = match self.store {
                Some(s) if self.remember && !c.one_shot => s.lookup(&c.action_hash).await,
                _ => None,
            };
            match settled {
                Some(Settled::Allow) => {
                    out.insert(c.call_id, remembered(true));
                }
                Some(Settled::Deny) => {
                    out.insert(c.call_id, remembered(false));
                }
                None => ask.push(c),
            }
        }
        (out, ask)
    }

    /// Ask a human about every call in `ask`, as ONE batch. Every call gets
    /// an entry (timeouts included). Call [`Self::recall`] first: this never
    /// consults the store.
    pub async fn ask(&self, ask: Vec<PendingCall>) -> HashMap<String, HitlDecision> {
        let mut out = HashMap::with_capacity(ask.len());
        if ask.is_empty() {
            return out;
        }
        let batch_id = uuid::Uuid::now_v7().to_string();
        let mut waiting = Vec::with_capacity(ask.len());
        let mut wire = Vec::with_capacity(ask.len());
        {
            let mut pa = self.approvals.lock().await;
            for c in ask {
                let hitl_id = uuid::Uuid::now_v7().to_string();
                let (tx, rx) = tokio::sync::oneshot::channel::<HitlDecision>();
                pa.insert(hitl_id.clone(), tx);
                if let Some(t) = self.shim_trust {
                    t.record_owner(&hitl_id, self.task_id);
                }
                if let Some(p) = self.parking {
                    let now = chrono::Utc::now();
                    p.park(super::parked::ParkedCall {
                        hitl_id: hitl_id.clone(),
                        task_id: self.task_id.to_string(),
                        tool_name: c.tool_name.clone(),
                        label: mur_common::hitl::label::decision_label(&c.tool_name, &c.tool_input),
                        parked_at: now,
                        expires_at: now
                            + chrono::Duration::from_std(self.timeout)
                                .unwrap_or(chrono::Duration::MAX),
                    });
                }
                wire.push(serde_json::json!({
                    "hitl_id": hitl_id,
                    "step_id": c.step_id,
                    "tool_name": c.tool_name,
                    "tool_input": c.tool_input,
                    "action_hash": c.action_hash,
                }));
                if let Some(risk) = c.risk
                    && let Some(obj) = wire.last_mut().and_then(|w| w.as_object_mut())
                {
                    obj.insert("risk".into(), serde_json::json!(risk));
                }
                waiting.push((c, hitl_id, rx));
            }
        }
        let first = &wire[0];
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool/approval_needed",
            "params": {
                "batch_id": batch_id,
                "task_id": self.task_id,
                "timeout_ms": self.timeout.as_millis() as u64,
                // Legacy single-call fields = the first call, for clients that
                // predate `calls`. Their answer lands on calls[0]; the rest
                // time out and deny, which is the fail-closed direction.
                "step_id": first["step_id"],
                "hitl_id": first["hitl_id"],
                "tool_name": first["tool_name"],
                "tool_input": first["tool_input"],
                "calls": wire,
            }
        });
        let mut notification = notification;
        if let Some(risk) = first.get("risk").cloned() {
            notification["params"]["risk"] = risk;
        }
        let _ = self.notifier.send(notification).await;

        // One deadline for the whole batch; each oneshot is awaited in turn
        // against what is left of it.
        let deadline = tokio::time::Instant::now() + self.timeout;
        for (c, hitl_id, rx) in waiting {
            let waited = tokio::time::timeout_at(deadline, rx).await;
            if let Some(t) = self.shim_trust {
                t.forget_owner(&hitl_id);
            }
            if let Some(p) = self.parking {
                p.unpark(&hitl_id);
            }
            let decision = match waited {
                Ok(Ok(d)) => d,
                _ => {
                    self.approvals.lock().await.remove(&hitl_id);
                    HitlDecision {
                        allow: false,
                        reason: Some(TIMED_OUT.into()),
                        surface: None,
                        awaiting_human: false,
                        refusal_code: None,
                    }
                }
            };
            // A timeout is not a human decision: recording it would deny the
            // next identical call without asking anyone. Only answers settle.
            let answered = decision.reason.as_deref() != Some(TIMED_OUT);
            if let (Some(s), true) = (self.store, answered && self.remember && !c.one_shot) {
                s.record(mur_common::hitl::HitlResponse {
                    hitl_id: hitl_id.clone(),
                    action_hash: c.action_hash.clone(),
                    allow: decision.allow,
                    reason: decision.reason.clone().unwrap_or_default(),
                    surface: decision.surface.clone().unwrap_or_else(|| "unknown".into()),
                    issued_at: Some(chrono::Utc::now()),
                    label: Some(mur_common::hitl::label::decision_label(
                        &c.tool_name,
                        &c.tool_input,
                    )),
                })
                .await;
            }
            out.insert(c.call_id, decision);
        }
        out
    }
}

fn remembered(allow: bool) -> HitlDecision {
    HitlDecision {
        allow,
        reason: Some(if allow {
            "approved earlier".into()
        } else {
            "denied earlier".into()
        }),
        surface: None,
        awaiting_human: false,
        refusal_code: None,
    }
}

/// Build the pending entry for one call. Separate so `task_runner` never
/// spells the hash itself.
pub fn pending(
    agent: &str,
    step_id: String,
    call: &crate::llm::ToolCallResult,
    risk: Option<mur_common::hitl::RiskTier>,
) -> PendingCall {
    PendingCall {
        risk,
        call_id: call.call_id.clone(),
        step_id,
        action_hash: chat_action_hash(&call.tool_name, &call.input, agent),
        tool_name: call.tool_name.clone(),
        tool_input: call.input.clone(),
        one_shot: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str) -> PendingCall {
        PendingCall {
            call_id: id.into(),
            step_id: format!("step-{id}"),
            tool_name: "bash".into(),
            tool_input: serde_json::json!({ "command": id }),
            action_hash: format!("hash-{id}"),
            risk: None,
            one_shot: false,
        }
    }

    /// Two gates on the SAME task, in flight at once, must not cross-answer.
    ///
    /// This is the property the MCP tool server depends on and the reason
    /// `2026-09-16-mur-tool-mcp-server-design.md` listed re-entrancy as an
    /// open question: an in-process turn resolves one batch at a time, but a
    /// spawned CLI issues each `tools/call` independently, so two batches for
    /// one task can be waiting together.
    ///
    /// It holds because the approvals map is keyed by a `hitl_id` minted per
    /// pending call inside `resolve`, not by `task_id` or `step_id` â and the
    /// lock is released before any await. The risk this guards against is a
    /// future key that looks more readable and is shared: keying by `task_id`
    /// would make these two gates answer each other, and nothing else in the
    /// suite would notice.
    #[tokio::test]
    async fn concurrent_gates_on_one_task_do_not_cross_answer() {
        let approvals: HitlApprovals = Default::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(8);

        // Answer whatever arrives: `allow` iff the tool_input said so. Each
        // notification carries its own hitl_id, which is the whole point.
        let responder = {
            let approvals = approvals.clone();
            tokio::spawn(async move {
                let mut answered = 0;
                while let Some(n) = rx.recv().await {
                    let calls = n["params"]["calls"].as_array().cloned().unwrap_or_default();
                    for c in calls {
                        let hitl_id = c["hitl_id"].as_str().expect("hitl_id").to_string();
                        let want_allow = c["tool_input"]["command"] == "yes";
                        if let Some(sender) = approvals.lock().await.remove(&hitl_id) {
                            let _ = sender.send(HitlDecision {
                                allow: want_allow,
                                reason: None,
                                surface: Some("test".into()),
                                awaiting_human: false,
                                refusal_code: None,
                            });
                            answered += 1;
                        }
                    }
                    if answered == 2 {
                        break;
                    }
                }
                answered
            })
        };

        // Same task_id on both â the collision case, not two unrelated tasks.
        let g1 = BatchGate {
            task_id: "task-1",
            timeout: Duration::from_secs(5),
            approvals: &approvals,
            notifier: &tx,
            store: None,
            remember: true,
            shim_trust: None,
            parking: None,
        };
        let g2 = BatchGate {
            task_id: "task-1",
            timeout: Duration::from_secs(5),
            approvals: &approvals,
            notifier: &tx,
            store: None,
            remember: true,
            shim_trust: None,
            parking: None,
        };
        let (a, b) = tokio::join!(g1.resolve(vec![call("yes")]), g2.resolve(vec![call("no")]));

        // Bounded: if a future change shares the key, one hitl_id never
        // arrives and the responder would otherwise wait forever. A guard
        // that hangs on regression is a guard that reports nothing.
        let answered = tokio::time::timeout(Duration::from_secs(5), responder)
            .await
            .expect("responder did not finish â a hitl_id was never delivered")
            .expect("responder panicked");
        assert_eq!(answered, 2);
        assert!(a["yes"].allow, "the allowing gate got the other's answer");
        assert!(!b["no"].allow, "the denying gate got the other's answer");
        assert!(
            approvals.lock().await.is_empty(),
            "every hitl_id should be consumed; a leak here grows unboundedly"
        );
    }

    /// #1600: the declared tier rides the wire â per call, and on the legacy
    /// top-level fields that plain mode reads â so every CLI gate decides on
    /// the same tier the runtime gated on. Absent = no rule declared one.
    #[tokio::test]
    async fn declared_risk_is_on_the_approval_request() {
        let approvals: HitlApprovals = Default::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(8);
        let gate = BatchGate {
            task_id: "t",
            timeout: Duration::from_millis(50),
            approvals: &approvals,
            notifier: &tx,
            store: None,
            remember: true,
            shim_trust: None,
            parking: None,
        };
        let mut risky = call("a");
        risky.risk = Some(mur_common::hitl::RiskTier::Destructive);
        let _ = gate.resolve(vec![risky, call("b")]).await;
        let n = rx.recv().await.expect("notification");
        assert_eq!(n["params"]["calls"][0]["risk"], "destructive");
        assert_eq!(n["params"]["risk"], "destructive", "legacy top-level field");
        assert!(n["params"]["calls"][1].get("risk").is_none());
    }

    /// Store that always remembers a deny and counts what is written to it.
    struct Remembers {
        records: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::hitl::store::DecisionStore for Remembers {
        async fn lookup(&self, _h: &str) -> Option<Settled> {
            Some(Settled::Deny)
        }
        async fn record(&self, _r: mur_common::hitl::HitlResponse) {
            self.records
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// #1783: a one-shot gate asks even when the store remembers a decision,
    /// and the human's answer is not written back as a standing decision.
    /// A remembering gate over the same store does neither.
    #[tokio::test]
    async fn one_shot_gate_always_asks_and_records_nothing() {
        let remembers = Arc::new(Remembers {
            records: Default::default(),
        });
        let store: Arc<dyn DecisionStore> = remembers.clone();
        for remember in [false, true] {
            let approvals: HitlApprovals = Default::default();
            let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(8);
            let answerer = approvals.clone();
            let answer = tokio::spawn(async move {
                let Some(n) = rx.recv().await else {
                    return false;
                };
                let id = n["params"]["hitl_id"].as_str().unwrap().to_string();
                let tx = answerer.lock().await.remove(&id).unwrap();
                let _ = tx.send(HitlDecision {
                    allow: true,
                    reason: None,
                    surface: Some("cli".into()),
                    awaiting_human: false,
                    refusal_code: None,
                });
                true
            });
            let gate = BatchGate {
                task_id: "t",
                timeout: Duration::from_secs(5),
                approvals: &approvals,
                notifier: &tx,
                store: Some(&store),
                remember,
                shim_trust: None,
                parking: None,
            };
            let out = gate.resolve(vec![call("a")]).await;
            drop(tx);
            let asked = answer.await.unwrap();
            assert_eq!(asked, !remember, "remember={remember}");
            assert_eq!(out["a"].allow, !remember, "remember={remember}");
            if !remember {
                assert_eq!(
                    remembers.records.load(std::sync::atomic::Ordering::SeqCst),
                    0,
                    "a one-shot answer must not be recorded"
                );
            }
        }
    }
}
