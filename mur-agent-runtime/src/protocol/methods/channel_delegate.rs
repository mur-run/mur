//! `channel/delegate` (v3d-2): a delegated specialist runs the sub-goal and
//! appends its OWN reply, signed by its own identity, attributed to Agent{self}.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mur_channel::ChannelService;
use mur_common::a2a::{Message, MessagePart};
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use serde_json::Value;

use crate::llm::RequestIntent;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::{TaskOutcome, TaskRunner, TaskSpec};

/// Derive the active fleet for a delegated turn from the (caller-supplied)
/// `channel_id`, **verified against the local fleet record**.
///
/// The channel id arrives in untrusted JSON-RPC params, so a string match on
/// `fleet-<name>` is not enough: a peer could dial with `channel_id="fleet-X"`
/// to make a non-member surface fleet-X-scoped skills (a confused deputy).
/// We therefore stamp `active_fleet` only when this agent is actually a member
/// (or the router) of an existing local fleet `<name>` — making `active_fleet`
/// a verified-local fact, like `active_project` (the cwd repo root). Any miss
/// (non-fleet channel, no such fleet on disk, not a member) yields `None`
/// (fail-closed), so fleet-scoped skills stay hidden outside their fleet.
fn verified_active_fleet(mur_home: &Path, agent: &str, channel_id: &str) -> Option<String> {
    let name = mur_common::fleet::fleet_name_from_channel_id(channel_id)?;
    let path = mur_home.join("fleets").join(name).join("fleet.yaml");
    let raw = std::fs::read_to_string(&path).ok()?;
    let fleet: mur_common::fleet::Fleet = serde_yaml_ng::from_str(&raw).ok()?;
    // Members and router are stored canonicalized (lowercase on-disk names), as
    // is `agent`, so an exact match is correct here.
    let is_member =
        fleet.members.iter().any(|m| m == agent) || fleet.router_or_concierge() == agent;
    is_member.then(|| name.to_string())
}

/// Derive the active team for a delegated turn from the fleet's `team_id` field.
///
/// Like `verified_active_fleet`, this reads the local fleet record rather than
/// accepting untrusted caller input. If the fleet has no `team_id`, or if the
/// channel is not a fleet channel, returns `None` (fail-closed).
fn verified_active_team(mur_home: &Path, channel_id: &str) -> Option<String> {
    let name = mur_common::fleet::fleet_name_from_channel_id(channel_id)?;
    let path = mur_home.join("fleets").join(name).join("fleet.yaml");
    let raw = std::fs::read_to_string(&path).ok()?;
    let fleet: mur_common::fleet::Fleet = serde_yaml_ng::from_str(&raw).ok()?;
    fleet.team_id
}

/// Append the specialist's reply to `channel_id` as `Agent{self}`, signed by the
/// specialist's identity (v3d-2 peer-writes-own).
#[allow(clippy::too_many_arguments)]
pub fn append_self_reply(
    mur_home: &Path,
    channel_id: &str,
    agent: &str,
    identity: &AgentIdentity,
    key_version: u32,
    reply_text: &str,
    task_id: &str,
    idem: Option<String>,
) -> anyhow::Result<()> {
    let svc = ChannelService::open(mur_home)?;
    svc.append_signed(
        channel_id,
        identity,
        key_version,
        ChannelActor::Agent {
            id: agent.to_string(),
        },
        EventKind::Message,
        serde_json::json!({ "text": reply_text, "task_id": task_id }),
        idem,
    )?;
    Ok(())
}

/// Extract the agent's reply text from a finished [`Task`](mur_common::a2a::Task):
/// the last message's first text part. Returns an empty string if the task has
/// no messages or the final message carries no text part.
pub(crate) fn reply_text_of(task: &mur_common::a2a::Task) -> String {
    task.messages
        .last()
        .and_then(|m| {
            m.parts.iter().find_map(|p| match p {
                MessagePart::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap_or_default()
}

/// `channel/delegate` handler. Runs the agent turn exactly like `message/send`
/// (non-streaming `run_sync` path), then ALSO appends the reply to the channel
/// as a signed `Agent{self}` event via [`append_self_reply`] before returning
/// the `Task` JSON. The self-append is best-effort: a failure is logged but does
/// not fail the RPC (the turn already succeeded).
pub struct ChannelDelegateHandler {
    runner: Arc<TaskRunner>,
    identity: Arc<AgentIdentity>,
    agent: String,
    key_version: u32,
    mur_home: PathBuf,
    /// `turn/heartbeat` cadence; the constant in production, shortened by tests.
    heartbeat_interval: std::time::Duration,
}

impl ChannelDelegateHandler {
    pub fn new(
        runner: Arc<TaskRunner>,
        identity: Arc<AgentIdentity>,
        agent: String,
        key_version: u32,
        mur_home: PathBuf,
    ) -> Self {
        Self {
            runner,
            identity,
            agent,
            key_version,
            mur_home,
            heartbeat_interval: crate::protocol::heartbeat::HEARTBEAT_INTERVAL,
        }
    }

    /// Shorten the beat so a test can see one inside its budget.
    #[cfg(test)]
    pub(crate) fn with_heartbeat_interval(mut self, every: std::time::Duration) -> Self {
        self.heartbeat_interval = every;
        self
    }
}

#[async_trait]
impl MethodHandler for ChannelDelegateHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        // NEW vs message/send: a channel_id is required so we know where to
        // append the self-reply.
        let channel_id = p
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| HandlerError::InvalidParams("missing channel_id".into()))?;
        let idem = p
            .get("idempotency_key")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Param parsing mirrors message/send: message → Message, optional
        // task_id and context.task_id → TaskSpec.
        let message: Message = serde_json::from_value(p["message"].clone())
            .map_err(|e| HandlerError::InvalidParams(format!("message: {e}")))?;
        let context_task_id = p
            .get("context")
            .and_then(|c| c.get("task_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        // Minted here when the caller did not supply one, because the turn has
        // to be marked unattended BEFORE it runs and the mark is keyed by task
        // id. `run_sync` honours a supplied id, so this is the same id the
        // reply carries.
        let task_id = p
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let spec = TaskSpec {
            cwd: None,
            input: message,
            context_task_id,
            task_id: Some(task_id.clone()),
            // A fleet's shared channel is `fleet-<name>`; derive (and verify
            // membership of) the active fleet so the runtime injects only this
            // agent's own fleet's fleet-scoped skills. Untrusted/non-member/
            // non-fleet channel ids yield None (fail-closed).
            active_fleet: verified_active_fleet(&self.mur_home, &self.agent, &channel_id),
            // Derive the team id from the fleet record (if any) so team-scoped
            // skills inject for fleet members belonging to that team (fail-closed).
            active_team: verified_active_team(&self.mur_home, &channel_id),
            // A fleet router/member is synchronously dialing this delegate and
            // waiting on the reply — Interactive, same as message/send.
            intent: RequestIntent::Interactive,
            // channel/delegate does NOT support output_artifact_path (fleets
            // use the channel itself as the artifact transport). Future: wire
            // through if a caller needs a file alongside the channel event.
            output_artifact_path: None,
            attended: false,
            deadline_secs: super::message_send::caller_deadline_secs(&p),
        };

        // Synchronous is not attended: the caller is a fleet router waiting on
        // a reply, not a human who can answer an approval prompt. Say so before
        // the turn runs, or a gated tool waits out `hitl.timeout_secs` against
        // a notifier nobody reads and the turn returns empty with nothing
        // naming approval as the cause.
        // The fleet router dialing this delegate holds a 90 s idle timeout
        // (proto ≥ 2); beat so a long specialist turn is not mistaken for a
        // dead one.
        let _beat = ctx.notifier.as_ref().map(|n| {
            crate::protocol::heartbeat::spawn(n.clone(), task_id.clone(), self.heartbeat_interval)
        });
        self.runner.mark_unattended(&task_id).await;

        // Run the turn (non-streaming path; v3d-2 does not need per-delta
        // forwarding for delegated specialist replies).
        let outcome = self.runner.run_sync(spec).await;
        self.runner.unregister_client_notifier(&task_id).await;
        // Only a Completed turn carries a genuine agent reply (`messages` =
        // [input, reply]); Failed/Cancelled tasks carry only the user input, so
        // appending their "last message" would sign the user's own text as the
        // specialist's reply. Skip the channel write for those — the RPC still
        // returns the Task so the caller sees the failure/cancellation.
        let completed = matches!(outcome, TaskOutcome::Completed(_));
        let task = match outcome {
            TaskOutcome::Completed(task)
            | TaskOutcome::Failed(task)
            | TaskOutcome::Cancelled(task) => task,
        };

        // Append the specialist's own signed reply to the channel. Best-effort:
        // a completed turn must still be returned even if the channel write
        // fails (e.g. channel missing / store error).
        if completed {
            let reply = reply_text_of(&task);
            if let Err(e) = append_self_reply(
                &self.mur_home,
                &channel_id,
                &self.agent,
                &self.identity,
                self.key_version,
                &reply,
                &task.id,
                idem,
            ) {
                tracing::warn!(
                    error = %e,
                    channel_id = %channel_id,
                    agent = %self.agent,
                    task_id = %task.id,
                    "channel/delegate: failed to append self-reply to channel"
                );
            }
        }

        serde_json::to_value(&task).map_err(|e| HandlerError::Internal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct NeverRunsTool {
        ran: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::tools::ToolExecutor for NeverRunsTool {
        fn name(&self) -> &str {
            "bash"
        }
        fn def(&self) -> crate::llm::ToolDef {
            crate::llm::ToolDef {
                name: "bash".into(),
                description: "test tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            self.ran.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(crate::tools::ToolOutput {
                text: "ran".into(),
                status: crate::tools::ToolStatus::Ok,
                images: Vec::new(),
            })
        }
    }

    /// The wiring, not the flag: `channel/delegate` must MARK its turn
    /// unattended, not merely be capable of it.
    ///
    /// The signal is time. The runner below has an agent-wide notifier and a
    /// 600 s approval timeout, so an unmarked turn parks on that notifier for
    /// ten minutes — which is the bug as users met it: a delegated step that
    /// returns empty with nothing naming approval. Marked, the gate answers
    /// before a prompt is ever built, so the whole call finishes inside the
    /// two-second budget here.
    #[tokio::test]
    async fn a_delegated_turn_marks_itself_unattended() {
        let tmp = TempDir::new().unwrap();
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tool_call = crate::llm::LlmResponse {
            text: String::new(),
            input_tokens: 1,
            output_tokens: 1,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: "c-1".into(),
                tool_name: "bash".into(),
                input: serde_json::json!({"command": "echo hi"}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        };
        let done = crate::llm::LlmResponse {
            text: "DONE".into(),
            input_tokens: 1,
            output_tokens: 1,
            model: "test".into(),
            tool_calls: vec![],
            stop_reason: crate::llm::StopReason::EndTurn,
        };
        // A notifier nobody reads: exactly the shape that turned the missing
        // mark into a ten-minute wait instead of an immediate refusal.
        let (ntx, _nrx) = tokio::sync::mpsc::channel(8);
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(crate::llm::stub::SequenceLlm::new(vec![
                tool_call, done,
            ])))
            .with_tools(vec![Arc::new(NeverRunsTool { ran: ran.clone() })])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "bash".into(),
                policy: mur_common::agent::ToolPolicy::Ask,
                risk: None,
            }])
            .with_pending_approvals(Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )))
            .with_notifier(ntx)
            .with_hitl_timeout_secs(600),
        );
        let handler = ChannelDelegateHandler::new(
            runner,
            Arc::new(AgentIdentity::generate()),
            "specialist".into(),
            1,
            tmp.path().to_path_buf(),
        );
        let params = serde_json::json!({
            "channel_id": "fleet-nope",
            "message": {"role": "user", "parts": [{"kind": "text", "text": "go"}]},
        });

        let out = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handler.handle(Some(params), &RequestContext::none()),
        )
        .await
        .expect("must not park on an approval nobody can answer");
        assert!(out.is_ok(), "the turn still returns a Task: {out:?}");
        assert!(
            !ran.load(std::sync::atomic::Ordering::Relaxed),
            "the gated tool must not execute"
        );
    }

    struct SlowOkTool;

    #[async_trait::async_trait]
    impl crate::tools::ToolExecutor for SlowOkTool {
        fn name(&self) -> &str {
            "bash"
        }
        fn def(&self) -> crate::llm::ToolDef {
            crate::llm::ToolDef {
                name: "bash".into(),
                description: "slow test tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            Ok(crate::tools::ToolOutput {
                text: "ran".into(),
                status: crate::tools::ToolStatus::Ok,
                images: Vec::new(),
            })
        }
    }

    /// The delegate beats on the connection that dialed it: a router waiting
    /// on a slow specialist sees a `turn/heartbeat` frame carrying the task id
    /// before the reply — the frame the 90 s idle timeout is counting on.
    #[tokio::test]
    async fn a_delegated_turn_beats_on_the_callers_connection() {
        let tmp = TempDir::new().unwrap();
        let tool_call = crate::llm::LlmResponse {
            text: String::new(),
            input_tokens: 1,
            output_tokens: 1,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: "c-1".into(),
                tool_name: "bash".into(),
                input: serde_json::json!({"command": "sleep"}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        };
        let done = crate::llm::LlmResponse {
            text: "DONE".into(),
            input_tokens: 1,
            output_tokens: 1,
            model: "test".into(),
            tool_calls: vec![],
            stop_reason: crate::llm::StopReason::EndTurn,
        };
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(crate::llm::stub::SequenceLlm::new(vec![
                tool_call, done,
            ])))
            .with_tools(vec![Arc::new(SlowOkTool)])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "bash".into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_pending_approvals(Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )))
            .with_hitl_timeout_secs(1),
        );
        let handler = ChannelDelegateHandler::new(
            runner,
            Arc::new(AgentIdentity::generate()),
            "specialist".into(),
            1,
            tmp.path().to_path_buf(),
        )
        .with_heartbeat_interval(std::time::Duration::from_millis(50));
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let params = serde_json::json!({
            "channel_id": "fleet-nope",
            "task_id": "t-beat",
            "message": {"role": "user", "parts": [{"kind": "text", "text": "go"}]},
        });
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handler.handle(Some(params), &RequestContext::with_notifier(tx)),
        )
        .await
        .expect("turn finishes");
        assert!(out.is_ok(), "{out:?}");
        let mut beats = 0;
        while let Ok(f) = rx.try_recv() {
            if f["method"] == crate::protocol::heartbeat::HEARTBEAT_METHOD {
                assert_eq!(f["params"]["task_id"], "t-beat");
                beats += 1;
            }
        }
        assert!(
            beats >= 1,
            "a 400 ms tool at a 50 ms beat must produce at least one frame"
        );
    }

    #[test]
    fn append_self_reply_is_signed_by_the_specialist() {
        let tmp = TempDir::new().unwrap();
        let id = AgentIdentity::generate();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        append_self_reply(tmp.path(), &ch.id, "qa", &id, 0, "the answer", "t-1", None).unwrap();
        let evs = svc.load_events(&ch.id).unwrap();
        let reply = evs
            .iter()
            .rev()
            .find(|e| {
                e.kind == EventKind::Message
                    && matches!(&e.actor, ChannelActor::Agent { id } if id == "qa")
            })
            .unwrap();
        assert_eq!(reply.payload["text"], "the answer");
        assert!(mur_channel::sign::verify_one(
            &ch.id,
            reply,
            &id.verifying_key_bytes(),
            true
        ));
    }

    fn write_fleet(home: &Path, name: &str, members: &[&str], router: Option<&str>) {
        write_fleet_with_team(home, name, members, router, None);
    }

    fn write_fleet_with_team(
        home: &Path,
        name: &str,
        members: &[&str],
        router: Option<&str>,
        team_id: Option<&str>,
    ) {
        let dir = home.join("fleets").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut yaml = format!("name: {name}\nchannel_id: fleet-{name}\nmembers:\n");
        for m in members {
            yaml.push_str(&format!("  - {m}\n"));
        }
        if let Some(r) = router {
            yaml.push_str(&format!("router: {r}\n"));
        }
        if let Some(t) = team_id {
            yaml.push_str(&format!("team_id: {t}\n"));
        }
        std::fs::write(dir.join("fleet.yaml"), yaml).unwrap();
    }

    #[test]
    fn verified_active_fleet_requires_membership() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        write_fleet(home, "dev", &["qa", "pm"], None);

        // member → Some
        assert_eq!(
            verified_active_fleet(home, "qa", "fleet-dev").as_deref(),
            Some("dev")
        );
        // non-member → None (confused-deputy defense): a crafted channel id for a
        // real fleet must not surface that fleet's skills to a non-member.
        assert_eq!(verified_active_fleet(home, "eve", "fleet-dev"), None);
        // non-fleet / malformed channel id → None
        assert_eq!(verified_active_fleet(home, "qa", "agent:foo:uuid"), None);
        assert_eq!(verified_active_fleet(home, "qa", "fleet-../etc"), None);
        // fleet not on disk → None (fail-closed)
        assert_eq!(verified_active_fleet(home, "qa", "fleet-ghost"), None);
    }

    #[test]
    fn verified_active_fleet_accepts_router() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // explicit router that is not in the members list
        write_fleet(home, "ops", &["qa"], Some("lead"));
        assert_eq!(
            verified_active_fleet(home, "lead", "fleet-ops").as_deref(),
            Some("ops")
        );
        // default router (concierge "mur") when none is set
        write_fleet(home, "sq", &["qa"], None);
        assert_eq!(
            verified_active_fleet(home, "mur", "fleet-sq").as_deref(),
            Some("sq")
        );
    }

    #[test]
    fn verified_active_team_reads_fleet_team_id() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();

        // fleet with a team_id → returns it
        write_fleet_with_team(home, "alpha", &["qa"], None, Some("org-x"));
        assert_eq!(
            verified_active_team(home, "fleet-alpha").as_deref(),
            Some("org-x")
        );

        // fleet without team_id → None
        write_fleet_with_team(home, "beta", &["qa"], None, None);
        assert_eq!(verified_active_team(home, "fleet-beta"), None);

        // non-fleet channel → None
        assert_eq!(verified_active_team(home, "agent:foo:uuid"), None);

        // fleet not on disk → None (fail-closed)
        assert_eq!(verified_active_team(home, "fleet-ghost"), None);
    }
}
