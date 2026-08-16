//! Product-level read contracts. Every surface renders these; no surface
//! recomputes grouping, classification, or ordering for itself.

use serde::{Deserialize, Serialize};

/// One row of the Chats inbox or the History drawer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    /// Agent participant ids. One = Direct, more than one = Group; the
    /// distinction is derived here, never stored.
    pub agents: Vec<String>,
    /// Derived from the first human message; may be empty for legacy or
    /// attachment-only conversations (render `{agent} · {date}` then).
    pub title: String,
    /// Text of the most recent message.
    pub preview: String,
    pub state: String,
    pub updated_at: String,
    /// Human-visible message count — NOT an unread badge.
    pub turns: usize,
    /// Messages after the read watermark. This is the badge.
    pub unread: usize,
    pub hitl_pending: bool,
}

/// What slice of conversations a caller wants.
///
/// Hub Chats rows and the mobile list use `{ agent: None, active_only: true }`;
/// the History drawer and TUI `/chats` use `{ agent: Some(x), active_only: false }`.
#[derive(Debug, Clone, Default)]
pub struct ConversationQuery {
    /// `None` = every agent.
    pub agent: Option<String>,
    /// Keep only the most recently updated conversation per agent.
    pub active_only: bool,
}

/// One row of the Work surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub title: String,
    /// `fleet-run` or `workflow-run`.
    pub kind: String,
    pub state: String,
    pub agents: Vec<String>,
    pub updated_at: String,
    pub hitl_pending: bool,
}

#[cfg(test)]
mod tests {
    use crate::ChannelService;
    use crate::summary::ConversationQuery;
    use mur_common::channel::{ChannelActor, EventKind};
    use tempfile::TempDir;

    fn say(svc: &ChannelService, id: &str, text: &str) {
        svc.append_message(
            id,
            ChannelActor::local_human(),
            EventKind::Message,
            text,
            None,
        )
        .unwrap();
    }

    #[test]
    fn active_only_returns_one_row_per_agent() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();

        let older = svc.create_for_agent("mur").unwrap();
        say(&svc, &older.id, "old question");
        let newer = svc.create_for_agent("mur").unwrap();
        say(&svc, &newer.id, "new question");
        let other = svc.create_for_agent("qa").unwrap();
        say(&svc, &other.id, "qa question");

        let rows = svc
            .list_conversations(ConversationQuery {
                agent: None,
                active_only: true,
            })
            .unwrap();

        assert_eq!(rows.len(), 2, "one row per agent, not per conversation");
        let mur = rows
            .iter()
            .find(|r| r.agents == vec!["mur".to_string()])
            .unwrap();
        assert_eq!(
            mur.id, newer.id,
            "the newest conversation is the active one"
        );
        assert_eq!(mur.title, "new question");
    }

    #[test]
    fn per_agent_history_returns_every_conversation_newest_first() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let a = svc.create_for_agent("mur").unwrap();
        say(&svc, &a.id, "first");
        let b = svc.create_for_agent("mur").unwrap();
        say(&svc, &b.id, "second");
        let unrelated = svc.create_for_agent("qa").unwrap();
        say(&svc, &unrelated.id, "qa");

        let rows = svc
            .list_conversations(ConversationQuery {
                agent: Some("mur".into()),
                active_only: false,
            })
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, b.id, "newest first");
        assert_eq!(rows[1].id, a.id);
    }

    #[test]
    fn fleet_and_workflow_channels_never_appear_in_conversations() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let fleet = svc
            .create_for_fleet("projectx", "mur", &["mur".into()])
            .unwrap();
        say(&svc, &fleet.id, "run it");
        let wf = svc.create_for_workflow("release").unwrap();
        say(&svc, &wf.id, "step 1");
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "hello");

        let rows = svc
            .list_conversations(ConversationQuery {
                agent: None,
                active_only: false,
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, chat.id);

        // The same agent's fleet run is reachable — just not from Chats.
        let runs = svc.list_runs().unwrap();
        assert_eq!(runs.len(), 2);
        assert!(
            runs.iter()
                .any(|r| r.id == fleet.id && r.kind == "fleet-run")
        );
        assert!(
            runs.iter()
                .any(|r| r.id == wf.id && r.kind == "workflow-run")
        );
    }

    #[test]
    fn active_only_ignores_the_agents_fleet_channel() {
        // Regression: `latest_for_agent` scans every channel a participant
        // appears in, so a recent fleet run used to shadow the real chat.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "hello");
        let fleet = svc
            .create_for_fleet("projectx", "mur", &["mur".into()])
            .unwrap();
        say(&svc, &fleet.id, "much later");

        let rows = svc
            .list_conversations(ConversationQuery {
                agent: Some("mur".into()),
                active_only: true,
            })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, chat.id);
    }

    #[test]
    fn empty_channels_are_never_listed() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        svc.create_for_agent("mur").unwrap(); // created, never written to

        let rows = svc
            .list_conversations(ConversationQuery {
                agent: None,
                active_only: false,
            })
            .unwrap();
        assert!(
            rows.is_empty(),
            "an abandoned draft must not appear in history"
        );
    }

    #[test]
    fn a_conversation_with_no_agent_is_not_shown_as_a_chat() {
        // Legacy workflow channels were created with zero participants, so an
        // inferred Conversation can have no agent. Showing it as a Direct chat
        // would be a row you cannot talk to; it stays reachable via advanced
        // channel tools instead.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let orphan = svc.create_for_workflow("legacy").unwrap();
        say(&svc, &orphan.id, "step 1");
        // Force the legacy shape: no purpose, no workflow title convention.
        let mut m = svc.store().load_manifest(&orphan.id).unwrap();
        m.purpose = None;
        m.title = "something".into();
        svc.store().save_manifest(&m).unwrap();
        svc.index().upsert(&m).unwrap();

        let rows = svc
            .list_conversations(ConversationQuery {
                agent: None,
                active_only: false,
            })
            .unwrap();
        assert!(rows.is_empty(), "a zero-agent conversation is not a chat");
    }

    #[test]
    fn a_group_conversation_does_not_hide_its_members_direct_chats() {
        // active_only means "the newest conversation per agent". A multi-agent
        // conversation is its own row and must not consume the slot of every
        // agent in it.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let direct = svc.create_for_agent("mur").unwrap();
        say(&svc, &direct.id, "direct question");
        let group = svc.create_for_agent("mur").unwrap();
        svc.add_participant(
            &group.id,
            "qa",
            mur_common::channel::ParticipantRole::Delegate,
        )
        .unwrap();
        say(&svc, &group.id, "group question");

        let rows = svc
            .list_conversations(ConversationQuery {
                agent: None,
                active_only: true,
            })
            .unwrap();

        assert_eq!(rows.len(), 2, "the group row plus mur's direct chat");
        assert!(rows.iter().any(|r| r.id == direct.id));
        assert!(rows.iter().any(|r| r.id == group.id));
    }

    #[test]
    fn a_summary_read_does_not_mutate_the_manifest() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "hello");
        let path = tmp
            .path()
            .join("channels")
            .join(&ch.id)
            .join("channel.yaml");
        let before = std::fs::read_to_string(&path).unwrap();

        let _ = svc
            .list_conversations(ConversationQuery {
                agent: None,
                active_only: false,
            })
            .unwrap();

        assert_eq!(before, std::fs::read_to_string(&path).unwrap());
    }
}
