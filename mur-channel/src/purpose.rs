//! The single owner of legacy channel classification.
//!
//! Channels written before `Channel.purpose` existed carry `None`. Exactly one
//! function resolves that for display, and it NEVER writes: a read path that
//! silently migrates data produces changes nobody can audit. On-disk correction
//! is the explicit `mur channel backfill-purpose` command.

use mur_common::channel::{Channel, ChannelPurpose};

/// Legacy titles created by `create_for_workflow` before purposes existed.
const WORKFLOW_TITLE_PREFIX: &str = "workflow: ";
/// Stable id prefix minted by `create_for_fleet`.
const FLEET_ID_PREFIX: &str = "fleet-";

/// Resolve a channel's purpose for display.
///
/// Order matters: an explicit stored purpose always wins, so a conversation
/// whose first message happens to start with "workflow:" can never be
/// reclassified once it has been written or backfilled.
pub fn effective_purpose(ch: &Channel) -> ChannelPurpose {
    if let Some(p) = ch.purpose {
        return p;
    }
    if ch.id.starts_with(FLEET_ID_PREFIX) {
        return ChannelPurpose::FleetRun;
    }
    if ch.title.starts_with(WORKFLOW_TITLE_PREFIX) {
        return ChannelPurpose::WorkflowRun;
    }
    ChannelPurpose::Conversation
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::channel::{ChannelActor, ChannelState};

    fn legacy(id: &str, title: &str) -> Channel {
        let now = Utc::now();
        Channel {
            v: 2,
            id: id.to_string(),
            title: title.to_string(),
            goal: Default::default(),
            state: ChannelState::Working,
            owner: ChannelActor::System,
            participants: vec![],
            purpose: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn explicit_purpose_always_wins_over_inference() {
        // An id that *looks* like a fleet but is explicitly a conversation.
        let mut ch = legacy("fleet-projectx", "fleet: projectx");
        ch.purpose = Some(ChannelPurpose::Conversation);
        assert_eq!(effective_purpose(&ch), ChannelPurpose::Conversation);
    }

    #[test]
    fn fleet_id_prefix_implies_fleet_run() {
        let ch = legacy("fleet-projectx", "fleet: projectx");
        assert_eq!(effective_purpose(&ch), ChannelPurpose::FleetRun);
    }

    #[test]
    fn workflow_title_convention_implies_workflow_run() {
        let ch = legacy("019ed0af-5e38-7912-b554-dc335a8fc2db", "workflow: release");
        assert_eq!(effective_purpose(&ch), ChannelPurpose::WorkflowRun);
    }

    #[test]
    fn everything_else_infers_conversation() {
        let ch = legacy("019ed0af-5e38-7912-b554-dc335a8fc2db", "chat with mur");
        assert_eq!(effective_purpose(&ch), ChannelPurpose::Conversation);
    }

    #[test]
    fn inference_is_deterministic_and_pure() {
        let ch = legacy("019ed0af-5e38-7912-b554-dc335a8fc2db", "workflow: release");
        let before = serde_json::to_string(&ch).unwrap();
        let a = effective_purpose(&ch);
        let b = effective_purpose(&ch);
        assert_eq!(a, b);
        assert_eq!(
            before,
            serde_json::to_string(&ch).unwrap(),
            "effective_purpose must not mutate the manifest"
        );
    }
}
