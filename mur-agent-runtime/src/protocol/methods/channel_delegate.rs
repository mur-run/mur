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
fn reply_text_of(task: &mur_common::a2a::Task) -> String {
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
        }
    }
}

#[async_trait]
impl MethodHandler for ChannelDelegateHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
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
        let task_id = p
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let spec = TaskSpec {
            input: message,
            context_task_id,
            task_id,
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
        };

        // Run the turn (non-streaming path; v3d-2 does not need per-delta
        // forwarding for delegated specialist replies).
        let outcome = self.runner.run_sync(spec).await;
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
