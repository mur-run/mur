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

/// Which surfaces a search should cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Conversations,
    Runs,
    All,
}

/// One match, located precisely enough to scroll to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub channel_id: String,
    /// `Some(seq)` locates the matching message (raw, 0-indexed — comparable
    /// to `last_seq`/`inbound_seqs`); `None` means only the title matched, so
    /// there is no event to scroll to.
    pub seq: Option<u64>,
    pub title: String,
    pub snippet: String,
    pub purpose: String,
    pub updated_at: String,
}

/// Grouped, never interleaved — a Chats hit and a Work hit mean different
/// things and are never presented as one undifferentiated list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResults {
    pub conversations: Vec<SearchHit>,
    pub runs: Vec<SearchHit>,
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

    #[test]
    fn rebuilding_the_index_reproduces_every_summary_field() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "the deploy pipeline broke");
        svc.append_message(
            &chat.id,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            "looking now",
            None,
        )
        .unwrap();
        let fleet = svc
            .create_for_fleet("projectx", "mur", &["mur".into()])
            .unwrap();
        say(&svc, &fleet.id, "run it");

        let q = || ConversationQuery {
            agent: None,
            active_only: false,
        };
        let before = svc.list_conversations(q()).unwrap();
        let runs_before = svc.list_runs().unwrap();
        let search_before = svc
            .search("pipeline", crate::summary::SearchScope::All)
            .unwrap();

        let n = svc.index().rebuild_from(svc.store()).unwrap();
        assert_eq!(n, 2, "both channels rebuilt");

        let after = svc.list_conversations(q()).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0].id, before[0].id);
        assert_eq!(after[0].title, before[0].title);
        assert_eq!(after[0].preview, before[0].preview);
        assert_eq!(after[0].turns, before[0].turns);
        assert_eq!(after[0].unread, before[0].unread);

        assert_eq!(svc.list_runs().unwrap().len(), runs_before.len());

        let search_after = svc
            .search("pipeline", crate::summary::SearchScope::All)
            .unwrap();
        assert_eq!(
            search_after.conversations.len(),
            search_before.conversations.len()
        );
        assert_eq!(
            search_after.conversations[0].seq,
            search_before.conversations[0].seq
        );
    }

    #[test]
    fn rebuilding_preserves_the_read_watermark() {
        // The watermark is the one derived value events cannot regenerate;
        // losing it on rebuild would resurface everything as unread.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "hi");
        let last = svc
            .append_message(
                &ch.id,
                ChannelActor::Agent { id: "mur".into() },
                EventKind::Message,
                "hello",
                None,
            )
            .unwrap();
        svc.mark_read(&ch.id, last.seq).unwrap();

        svc.index().rebuild_from(svc.store()).unwrap();

        let q = ConversationQuery {
            agent: None,
            active_only: false,
        };
        assert_eq!(svc.list_conversations(q).unwrap()[0].unread, 0);
    }
}

#[cfg(test)]
mod search_tests {
    use crate::ChannelService;
    use crate::summary::SearchScope;
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
    fn search_finds_message_text_and_locates_the_exact_event() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "opening question");
        say(&svc, &ch.id, "the deploy pipeline broke again");

        let res = svc.search("pipeline", SearchScope::All).unwrap();
        assert_eq!(res.conversations.len(), 1);
        assert_eq!(res.conversations[0].channel_id, ch.id);
        assert_eq!(
            res.conversations[0].seq,
            Some(1),
            "second message of two; seqs are 0-indexed"
        );
        assert!(res.conversations[0].snippet.contains("pipeline"));
    }

    #[test]
    fn results_are_grouped_by_surface_not_interleaved() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "shared keyword here");
        let fleet = svc
            .create_for_fleet("projectx", "mur", &["mur".into()])
            .unwrap();
        say(&svc, &fleet.id, "shared keyword there");

        let res = svc.search("keyword", SearchScope::All).unwrap();
        assert_eq!(res.conversations.len(), 1);
        assert_eq!(res.runs.len(), 1);
        assert_eq!(res.conversations[0].channel_id, chat.id);
        assert_eq!(res.runs[0].channel_id, fleet.id);
    }

    #[test]
    fn scope_filters_the_result_set() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "shared keyword here");
        let fleet = svc
            .create_for_fleet("projectx", "mur", &["mur".into()])
            .unwrap();
        say(&svc, &fleet.id, "shared keyword there");

        let only_chats = svc.search("keyword", SearchScope::Conversations).unwrap();
        assert_eq!(only_chats.conversations.len(), 1);
        assert!(only_chats.runs.is_empty());
    }

    #[test]
    fn search_matches_titles_as_well_as_bodies() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "refactor the parser"); // becomes the title
        say(&svc, &ch.id, "unrelated follow-up");

        let res = svc.search("refactor", SearchScope::Conversations).unwrap();
        assert_eq!(res.conversations.len(), 1);
    }

    #[test]
    fn a_title_only_match_reports_no_event_to_scroll_to() {
        // The fleet title is minted at creation, so "projectx" appears in the
        // title but in no message body — the one way to reach the title-only arm.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let fleet = svc
            .create_for_fleet("projectx", "mur", &["mur".into()])
            .unwrap();
        say(&svc, &fleet.id, "run it");

        let res = svc.search("projectx", SearchScope::All).unwrap();
        assert_eq!(res.runs.len(), 1);
        assert_eq!(res.runs[0].channel_id, fleet.id);
        assert_eq!(
            res.runs[0].seq, None,
            "title matched; there is no event to scroll to"
        );
    }

    #[test]
    fn a_query_with_fts_syntax_characters_does_not_error() {
        // User input goes straight into a MATCH expression; unescaped quotes
        // and operators would otherwise be a hard error mid-typing.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "quoted \"thing\" and (parens)");

        for q in ["\"", "AND", "a OR", "(", "*", ""] {
            let res = svc.search(q, SearchScope::All);
            assert!(res.is_ok(), "query {q:?} must not error: {:?}", res.err());
        }
    }

    #[test]
    fn search_does_not_mutate_manifests() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "hello world");
        let path = tmp
            .path()
            .join("channels")
            .join(&ch.id)
            .join("channel.yaml");
        let before = std::fs::read_to_string(&path).unwrap();

        let _ = svc.search("hello", SearchScope::All).unwrap();

        assert_eq!(before, std::fs::read_to_string(&path).unwrap());
    }
}
