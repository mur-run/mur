//! Events → the history a channel conversation replays (§3.4).
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

use super::{ProjectedMessage, Role};

/// Which events become history: `Message` by `Human` or `Agent{self}`, plus
/// (assumption S1) the `System` `Note` murmur persists for a `!cmd` card,
/// folded as a user turn so the reply after it has one. The repair record is
/// also a `System` `Note`, but it is the chain's bookkeeping, not a turn, so
/// it is skipped. Every other kind/actor is skipped. Only `seq < before`.
pub fn project(events: &[ChannelEvent], agent: &str, before: u64) -> Vec<ProjectedMessage> {
    events
        .iter()
        .filter(|e| e.seq < before)
        .filter_map(|e| {
            let text = e.payload.get("text").and_then(|t| t.as_str())?.to_string();
            let role = match (&e.actor, e.kind) {
                (ChannelActor::Human { .. }, EventKind::Message) => Role::User,
                (ChannelActor::Agent { id }, EventKind::Message) if id == agent => Role::Agent,
                (ChannelActor::System, EventKind::Note)
                    if !crate::repair_record::is_repair_record(e) =>
                {
                    Role::User
                }
                _ => return None,
            };
            Some(ProjectedMessage {
                seq: e.seq,
                role,
                text,
            })
        })
        .collect()
}
