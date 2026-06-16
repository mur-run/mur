//! `channel/delegate` (v3d-2): a delegated specialist runs the sub-goal and
//! appends its OWN reply, signed by its own identity, attributed to Agent{self}.

use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;

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
}
