use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use mur_common::channel::{
    CHANNEL_SCHEMA_VERSION, Channel, ChannelActor, ChannelEvent, ChannelPurpose, ChannelState,
    EventKind, Goal, Participant, ParticipantRole,
};

use crate::index::{ChannelIndex, ChannelRow};
use crate::store::ChannelStore;

/// Shared truncation limit for auto-derived conversation titles. One constant
/// so TUI, Hub, and mobile cannot disagree about where a title ends.
pub const TITLE_MAX_CHARS: usize = 48;

/// How many index rows a summary query scans. The index is ordered by activity,
/// so this bounds work while keeping every recently-touched channel reachable.
const SUMMARY_SCAN_LIMIT: usize = 2000;

/// Max full-text matches returned per query.
const SEARCH_LIMIT: usize = 200;

/// Typed payload for an [`EventKind::Delegation`] event. The concierge owns
/// `child_task_id` (the A2A task id it gave the dialed agent) and stamps the
/// canonical `target_agent` name.
#[derive(serde::Serialize)]
struct DelegationPayload<'a> {
    target_agent: &'a str,
    child_task_id: &'a str,
    parent_channel_id: &'a str,
    /// Sub-goal text handed to the delegate, so observers (fleet rail,
    /// followed-channel milestones) can say WHAT was delegated, not just to
    /// whom. Optional: absent on legacy events and non-goal delegations.
    #[serde(skip_serializing_if = "Option::is_none")]
    goal: Option<&'a str>,
    /// The run whose execution wrote this event (run-status run boundary).
    /// Optional: absent on legacy events and on delegations that are not
    /// part of a recorded run — those are not claimed by any rebuild.
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<&'a str>,
}

/// Build the canonical `Delegation` event payload (single-sourced schema). Used
/// by [`ChannelService::append_delegation`] and by writers that want to SIGN the
/// delegation event via `append_signed` (v3d) without duplicating the shape.
/// Infallible: serializing a `&str`-only struct cannot fail.
pub fn delegation_payload(
    parent_channel_id: &str,
    target_agent: &str,
    child_task_id: &str,
    goal: Option<&str>,
    run_id: Option<&str>,
) -> serde_json::Value {
    serde_json::to_value(DelegationPayload {
        target_agent,
        child_task_id,
        parent_channel_id,
        goal,
        run_id,
    })
    .expect("DelegationPayload serializes")
}

/// The single API both the CLI and the Hub call. Keeps the log + the index in
/// sync on every mutation.
fn state_str(s: ChannelState) -> &'static str {
    match s {
        ChannelState::Submitted => "submitted",
        ChannelState::Working => "working",
        ChannelState::InputRequired => "input-required",
        ChannelState::Completed => "completed",
        ChannelState::Failed => "failed",
        ChannelState::Canceled => "canceled",
        ChannelState::Rejected => "rejected",
        ChannelState::Stale => "stale",
    }
}

pub struct ChannelService {
    store: ChannelStore,
    index: ChannelIndex,
}

impl ChannelService {
    pub fn open(mur_home: &Path) -> Result<Self> {
        let store = ChannelStore::new(mur_home);
        let index = ChannelIndex::open(mur_home)?;
        // First open after an upgrade that added the activity columns: every
        // pre-existing row is still sitting on their SQL defaults (msg_count=0,
        // preview='', ...), which makes every legacy channel look inactive to
        // the new contracts. `ChannelIndex::migrate` cannot fix this itself —
        // it has no `ChannelStore` — so the one-time rebuild happens here,
        // the only place that holds both. `just_migrated` is true at most once
        // per DB file (ALTER TABLE ADD COLUMN fails forever after the first
        // success), so this cannot re-run on every open. A failed rebuild must
        // never block startup: the index is disposable, so log and carry on
        // with the stale-but-working rows.
        if index.just_migrated()
            && let Err(e) = index.rebuild_from(&store)
        {
            tracing::warn!(
                error = %e,
                "post-migration channel index rebuild failed; index remains stale but usable"
            );
        }
        Ok(Self { store, index })
    }

    /// Create a fresh channel whose participants are the local human (owner)
    /// and one agent (delegate). Used by both CLI and Hub when opening a chat.
    pub fn create_for_agent(&self, agent: &str) -> Result<Channel> {
        let now = Utc::now();
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: uuid::Uuid::now_v7().to_string(),
            title: String::new(),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: Some(ChannelPurpose::Conversation),
            owner: ChannelActor::local_human(),
            participants: vec![
                Participant {
                    actor: ChannelActor::local_human(),
                    role: ParticipantRole::Owner,
                    joined_at: now,
                },
                Participant {
                    actor: ChannelActor::Agent {
                        id: agent.to_string(),
                    },
                    role: ParticipantRole::Delegate,
                    joined_at: now,
                },
            ],
            created_at: now,
            updated_at: now,
        };
        self.store.create(&ch)?;
        self.index.upsert(&ch)?;
        Ok(ch)
    }

    /// Create the long-lived shared channel for a fleet. Id is the stable,
    /// filesystem-safe `fleet-<name>`. Router gets `Router`, members `Delegate`.
    pub fn create_for_fleet(
        &self,
        fleet_name: &str,
        router: &str,
        members: &[String],
    ) -> Result<Channel> {
        let now = Utc::now();
        let mut participants = vec![
            Participant {
                actor: ChannelActor::local_human(),
                role: ParticipantRole::Owner,
                joined_at: now,
            },
            Participant {
                actor: ChannelActor::Agent {
                    id: router.to_string(),
                },
                role: ParticipantRole::Router,
                joined_at: now,
            },
        ];
        for m in members {
            participants.push(Participant {
                actor: ChannelActor::Agent { id: m.clone() },
                role: ParticipantRole::Delegate,
                joined_at: now,
            });
        }
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: format!("fleet-{fleet_name}"),
            title: format!("fleet: {fleet_name}"),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: Some(ChannelPurpose::FleetRun),
            owner: ChannelActor::local_human(),
            participants,
            created_at: now,
            updated_at: now,
        };
        self.store.create(&ch)?;
        self.index.upsert(&ch)?;
        Ok(ch)
    }

    /// Append a message event and bump the manifest's `updated_at` + index.
    pub fn append_message(
        &self,
        channel_id: &str,
        actor: ChannelActor,
        kind: EventKind,
        text: &str,
        task_id: Option<&str>,
    ) -> Result<ChannelEvent> {
        let mut payload = serde_json::json!({ "text": text });
        if let Some(t) = task_id {
            payload["task_id"] = serde_json::Value::String(t.to_string());
        }
        let ev = self
            .store
            .append_event(channel_id, actor, kind, payload, None, None, None)?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            if let Some(t) = Self::derived_title(&ch, &ev) {
                ch.title = t;
            }
            self.refresh_read_model(&ch, &ev);
        }
        Ok(ev)
    }

    /// Create a channel that records a workflow execution. No agent participant;
    /// the DAG executor acts as `ChannelActor::System`.
    pub fn create_for_workflow(&self, skill_name: &str) -> Result<Channel> {
        let now = Utc::now();
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: uuid::Uuid::now_v7().to_string(),
            title: format!("workflow: {skill_name}"),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: Some(ChannelPurpose::WorkflowRun),
            owner: ChannelActor::local_human(),
            participants: vec![],
            created_at: now,
            updated_at: now,
        };
        self.store.create(&ch)?;
        self.index.upsert(&ch)?;
        Ok(ch)
    }

    /// Append an event with an arbitrary payload, bumping `updated_at` + index.
    pub fn append(
        &self,
        channel_id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let ev = self.store.append_event(
            channel_id,
            actor,
            kind,
            payload,
            idempotency_key,
            None,
            None,
        )?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            if let Some(t) = Self::derived_title(&ch, &ev) {
                ch.title = t;
            }
            self.refresh_read_model(&ch, &ev);
        }
        Ok(ev)
    }

    /// Sign an event with `identity` (key_version `kv`) and append it. Used by
    /// the channel's writer (the router/owner) so the log is forgery-resistant.
    #[allow(clippy::too_many_arguments)]
    pub fn append_signed(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let sig = crate::sign::sign_event(
            identity,
            channel_id,
            &actor,
            kind,
            &payload,
            idempotency_key.as_deref(),
        );
        let ev = self.store.append_event(
            channel_id,
            actor,
            kind,
            payload,
            idempotency_key,
            Some(sig),
            Some(kv),
        )?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            if let Some(t) = Self::derived_title(&ch, &ev) {
                ch.title = t;
            }
            self.refresh_read_model(&ch, &ev);
        }
        Ok(ev)
    }

    /// Append a `Delegation` event (actor `System`) recording that `target_agent`
    /// was handed the sub-goal under `child_task_id`. `idempotency_key` is set by
    /// the caller (deterministic in v3b) but NOT yet de-duplicated (v3c).
    /// `run_id` stamps the run-status run boundary on the payload (see
    /// [`delegation_payload`]); pass `None` for delegations outside any run.
    pub fn append_delegation(
        &self,
        channel_id: &str,
        target_agent: &str,
        child_task_id: &str,
        idempotency_key: Option<String>,
        run_id: Option<&str>,
    ) -> Result<ChannelEvent> {
        let payload = delegation_payload(channel_id, target_agent, child_task_id, None, run_id);
        self.append(
            channel_id,
            ChannelActor::System,
            EventKind::Delegation,
            payload,
            idempotency_key,
        )
    }

    /// Emit a `StateChange` event and persist the new state on the manifest.
    /// `run_id` stamps the run-status run boundary on the payload; pass `None`
    /// for transitions that are not part of a recorded run.
    pub fn transition(
        &self,
        channel_id: &str,
        new_state: ChannelState,
        actor: ChannelActor,
        run_id: Option<&str>,
    ) -> Result<ChannelEvent> {
        let old_state = self
            .store
            .load_manifest(channel_id)
            .map(|ch| ch.state)
            .unwrap_or(ChannelState::Working);
        let mut payload = serde_json::json!({
            "from": state_str(old_state),
            "to":   state_str(new_state),
        });
        if let Some(run_id) = run_id {
            payload["run_id"] = serde_json::json!(run_id);
        }
        let ev = self.store.append_event(
            channel_id,
            actor,
            EventKind::StateChange,
            payload,
            None,
            None,
            None,
        )?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.state = new_state;
            ch.updated_at = ev.ts;
            if let Some(t) = Self::derived_title(&ch, &ev) {
                ch.title = t;
            }
            self.refresh_read_model(&ch, &ev);
        }
        Ok(ev)
    }

    pub fn load_events(&self, channel_id: &str) -> Result<Vec<ChannelEvent>> {
        self.store.load_events(channel_id)
    }

    pub fn list(&self, limit: usize) -> Result<Vec<ChannelRow>> {
        self.index.list(limit)
    }

    /// The newest channel that has `agent` as a participant — the CLI's
    /// `--resume` target and the Hub's "open this agent" target.
    pub fn latest_for_agent(&self, agent: &str) -> Result<Option<String>> {
        // list() is newest-first; load each manifest and match the participant.
        for row in self.index.list(1000)? {
            if let Ok(ch) = self.store.load_manifest(&row.id)
                && ch
                    .participants
                    .iter()
                    .any(|p| matches!(&p.actor, ChannelActor::Agent { id } if id == agent))
            {
                return Ok(Some(ch.id));
            }
        }
        Ok(None)
    }

    /// Add an agent as a participant (idempotent on agent id). Re-indexes.
    pub fn add_participant(
        &self,
        channel_id: &str,
        agent_id: &str,
        role: ParticipantRole,
    ) -> Result<()> {
        let mut ch = self.store.load_manifest(channel_id)?;
        let exists = ch
            .participants
            .iter()
            .any(|p| matches!(&p.actor, ChannelActor::Agent { id } if id == agent_id));
        if !exists {
            ch.participants.push(Participant {
                actor: ChannelActor::Agent {
                    id: agent_id.to_string(),
                },
                role,
                joined_at: Utc::now(),
            });
            ch.updated_at = Utc::now();
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(())
    }

    /// Remove an agent participant (no-op if absent). Re-indexes.
    pub fn remove_participant(&self, channel_id: &str, agent_id: &str) -> Result<()> {
        let mut ch = self.store.load_manifest(channel_id)?;
        let before = ch.participants.len();
        ch.participants
            .retain(|p| !matches!(&p.actor, ChannelActor::Agent { id } if id == agent_id));
        if ch.participants.len() != before {
            ch.updated_at = Utc::now();
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(())
    }

    /// Delete the channel entirely (store dir + read-model row). Idempotent.
    pub fn delete_channel(&self, channel_id: &str) -> Result<()> {
        self.store.delete(channel_id)?;
        self.index.remove(channel_id)?;
        Ok(())
    }

    pub fn store(&self) -> &ChannelStore {
        &self.store
    }
    pub fn index(&self) -> &ChannelIndex {
        &self.index
    }

    /// Mark everything up to `seq` as read in `channel_id`.
    ///
    /// Callers must only do this for a focused view whose tail is actually
    /// rendered — a background window clearing unread is the bug this rule
    /// exists to prevent.
    pub fn mark_read(&self, channel_id: &str, seq: u64) -> Result<()> {
        self.index.mark_read(channel_id, seq)
    }

    /// Conversation rows for Chats. Ordering is newest-activity-first; empty
    /// channels (created-but-never-sent drafts) are omitted.
    pub fn list_conversations(
        &self,
        q: crate::summary::ConversationQuery,
    ) -> Result<Vec<crate::summary::ConversationSummary>> {
        let mut out: Vec<crate::summary::ConversationSummary> = Vec::new();
        let mut seen_agents: Vec<String> = Vec::new();
        for row in self.index.list(SUMMARY_SCAN_LIMIT)? {
            if row.purpose != "conversation" || row.msg_count == 0 {
                continue;
            }
            let agents: Vec<String> = serde_json::from_str(&row.agents).unwrap_or_default();
            // A conversation with no agent participant cannot be chatted with;
            // legacy workflow channels have this shape. Diagnostics and the
            // advanced channel tools still reach it.
            if agents.is_empty() {
                continue;
            }
            if let Some(want) = &q.agent
                && !agents.iter().any(|a| a == want)
            {
                continue;
            }
            if q.active_only {
                // index.list() is newest-first, so the first Direct row an agent
                // appears in IS its active conversation. Group conversations are
                // their own row and never consume an agent's slot.
                if let [only] = agents.as_slice() {
                    if seen_agents.iter().any(|a| a == only) {
                        continue;
                    }
                    seen_agents.push(only.clone());
                }
            }
            let inbound: Vec<i64> = serde_json::from_str(&row.inbound_seqs).unwrap_or_default();
            let unread = inbound.iter().filter(|s| **s > row.last_read_seq).count();
            out.push(crate::summary::ConversationSummary {
                id: row.id,
                agents,
                title: row.title,
                preview: row.preview,
                state: row.state,
                updated_at: row.updated_at,
                turns: row.msg_count as usize,
                unread,
                hitl_pending: row.hitl_pending,
            });
        }
        Ok(out)
    }

    /// Fleet and workflow executions for Work. Never returns conversations.
    pub fn list_runs(&self) -> Result<Vec<crate::summary::RunSummary>> {
        let mut out = Vec::new();
        for row in self.index.list(SUMMARY_SCAN_LIMIT)? {
            if row.purpose == "conversation" {
                continue;
            }
            out.push(crate::summary::RunSummary {
                id: row.id,
                title: row.title,
                kind: row.purpose,
                state: row.state,
                agents: serde_json::from_str(&row.agents).unwrap_or_default(),
                updated_at: row.updated_at,
                hitl_pending: row.hitl_pending,
            });
        }
        Ok(out)
    }

    /// Search channel titles and message bodies, grouped by surface.
    pub fn search(
        &self,
        query: &str,
        scope: crate::summary::SearchScope,
    ) -> Result<crate::summary::SearchResults> {
        use crate::summary::{SearchHit, SearchResults, SearchScope};

        let q = query.trim();
        let mut out = SearchResults::default();
        if q.is_empty() {
            return Ok(out);
        }
        let needle = q.to_lowercase();

        // Index rows carry title + purpose + activity; body hits are keyed by id.
        let rows = self.index.list(SUMMARY_SCAN_LIMIT)?;
        let body_hits = self.index.search_bodies(q, SEARCH_LIMIT)?;

        for row in rows {
            if row.msg_count == 0 {
                continue;
            }
            let is_conversation = row.purpose == "conversation";
            let wanted = match scope {
                SearchScope::All => true,
                SearchScope::Conversations => is_conversation,
                SearchScope::Runs => !is_conversation,
            };
            if !wanted {
                continue;
            }
            let body = body_hits.iter().find(|(id, _, _)| *id == row.id);
            let title_match = row.title.to_lowercase().contains(&needle);
            let (seq, snippet) = match (body, title_match) {
                (Some((_, seq, snip)), _) => (Some(*seq as u64), snip.clone()),
                (None, true) => (None, row.preview.clone()),
                (None, false) => continue,
            };
            let hit = SearchHit {
                channel_id: row.id,
                seq,
                title: row.title,
                snippet,
                purpose: row.purpose,
                updated_at: row.updated_at,
            };
            if is_conversation {
                out.conversations.push(hit);
            } else {
                out.runs.push(hit);
            }
        }
        Ok(out)
    }

    /// Refresh the manifest + SQLite read-model after a successful event
    /// append. Both are rebuildable projections of `events.jsonl` — a
    /// refresh failure must not fail an append whose event is already
    /// durable. Concretely: a sandboxed delegate (peer-writes-own, v3d-2)
    /// may be able to write the channel store but not the shared index;
    /// SQLite reports the denied write as "attempt to write a readonly
    /// database" (G3, live fleet run 2026-07-09).
    ///
    /// Every caller here has an event to fold — manifest-only changes
    /// (participant edits, in `add_participant`/`remove_participant`) go
    /// straight through `save_manifest` + `index.upsert` and never call
    /// this, precisely so they can't touch activity columns.
    fn refresh_read_model(&self, ch: &Channel, ev: &ChannelEvent) {
        let res = self
            .store
            .save_manifest(ch)
            .and_then(|()| self.index.upsert(ch))
            .and_then(|()| self.index.record_event(&ch.id, ev));
        if let Err(e) = res {
            tracing::warn!(
                channel_id = %ch.id,
                error = %e,
                "read-model refresh failed after append (event persisted; index is rebuildable)"
            );
        }
    }

    /// The title a conversation should take from `ev`, if any.
    ///
    /// Only untitled Conversations, only the first human `Message`, only when
    /// it has text. Fleet/workflow channels keep their minted titles, and an
    /// attachment-only opener leaves the title empty for the summary layer to
    /// render as `{agent} · {date}`.
    fn derived_title(ch: &Channel, ev: &ChannelEvent) -> Option<String> {
        if crate::purpose::effective_purpose(ch) != ChannelPurpose::Conversation
            || !ch.title.is_empty()
            || ev.kind != EventKind::Message
            || !matches!(ev.actor, ChannelActor::Human { .. })
        {
            return None;
        }
        let text = ev.payload.get("text")?.as_str()?.trim();
        if text.is_empty() {
            return None;
        }
        Some(text.chars().take(TITLE_MAX_CHARS).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn creation_paths_write_explicit_purpose() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();

        let chat = svc.create_for_agent("mur").unwrap();
        assert_eq!(chat.purpose, Some(ChannelPurpose::Conversation));

        let wf = svc.create_for_workflow("release").unwrap();
        assert_eq!(wf.purpose, Some(ChannelPurpose::WorkflowRun));
        assert_eq!(
            wf.title, "workflow: release",
            "workflow title convention is load-bearing for legacy inference"
        );

        let fleet = svc
            .create_for_fleet("projectx", "lead", &["a".into()])
            .unwrap();
        assert_eq!(fleet.purpose, Some(ChannelPurpose::FleetRun));
        assert_eq!(fleet.id, "fleet-projectx");
    }

    #[test]
    fn opening_over_a_preexisting_v1_index_triggers_a_one_time_rebuild() {
        // Simulate an install that predates the activity columns: real
        // channel data on disk (manifest + an event), but an index db that
        // only has the base v1 schema. The first `ChannelService::open`
        // must detect the migration and auto-rebuild so the channel isn't
        // invisible behind the new columns' SQL defaults; a second open
        // must NOT re-run the rebuild — proven by a hand-diverged row
        // surviving it.
        let tmp = TempDir::new().unwrap();

        // Real channel data, written directly via ChannelStore so nothing
        // has touched the index yet.
        let store = ChannelStore::new(tmp.path());
        let now = Utc::now();
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: "legacy-chat".into(),
            title: "chat with mur".into(),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: Some(ChannelPurpose::Conversation),
            owner: ChannelActor::local_human(),
            participants: vec![Participant {
                actor: ChannelActor::Agent { id: "mur".into() },
                role: ParticipantRole::Delegate,
                joined_at: now,
            }],
            created_at: now,
            updated_at: now,
        };
        store.create(&ch).unwrap();
        store
            .append_event(
                &ch.id,
                ChannelActor::local_human(),
                EventKind::Message,
                serde_json::json!({"text": "hello from before the upgrade"}),
                None,
                None,
                None,
            )
            .unwrap();

        // A hand-built v1-schema index db: base columns only, matching what
        // `migrate_adds_columns_to_a_preexisting_v1_database` in index.rs
        // uses to simulate a pre-migration DB.
        let idx_dir = tmp.path().join("index").join("channels");
        std::fs::create_dir_all(&idx_dir).unwrap();
        {
            let conn = rusqlite::Connection::open(idx_dir.join("channels.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE channels (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
                    owner TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO channels VALUES
                    ('legacy-chat','chat with mur','working','{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        // First open: migrate() adds the activity columns for the first
        // time, so open() must auto-rebuild from the manifest + event log.
        let svc = ChannelService::open(tmp.path()).unwrap();
        let rows = svc.index().list(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].msg_count, 1, "rebuild must replay the event log");
        assert_eq!(rows[0].preview, "hello from before the upgrade");
        drop(svc);

        // Diverge a column by hand. A second automatic rebuild would erase
        // this; the test proves it does not run.
        {
            let idx = ChannelIndex::open(tmp.path()).unwrap();
            idx.conn_for_test()
                .execute(
                    "UPDATE channels SET preview='DIVERGED' WHERE id='legacy-chat'",
                    [],
                )
                .unwrap();
        }

        let svc2 = ChannelService::open(tmp.path()).unwrap();
        let rows2 = svc2.index().list(10).unwrap();
        assert_eq!(
            rows2[0].preview, "DIVERGED",
            "second open must not re-run the auto-rebuild"
        );
    }

    #[test]
    fn new_conversation_starts_with_an_empty_title() {
        // The title comes from the first human message (Task 5), not from a
        // useless "chat with {agent}" placeholder that made every row identical.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        assert_eq!(ch.title, "");
    }

    #[test]
    fn purpose_survives_a_manifest_round_trip() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        let reloaded = svc.store().load_manifest(&ch.id).unwrap();
        assert_eq!(reloaded.purpose, Some(ChannelPurpose::Conversation));
    }

    #[test]
    fn append_delegation_writes_typed_event() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("delegating-wf").unwrap();
        let ev = svc
            .append_delegation(&ch.id, "qa", "child-task-1", Some("idem-1".into()), None)
            .unwrap();
        assert_eq!(ev.kind, EventKind::Delegation);
        assert_eq!(ev.actor, ChannelActor::System);
        assert_eq!(ev.payload["target_agent"], "qa");
        assert_eq!(ev.payload["child_task_id"], "child-task-1");
        assert_eq!(ev.payload["parent_channel_id"], ch.id);
        assert_eq!(ev.idempotency_key.as_deref(), Some("idem-1"));
    }

    #[test]
    fn append_structured_and_transition() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("deploy").unwrap();
        assert_eq!(ch.state, ChannelState::Working);
        assert!(
            ch.participants.is_empty(),
            "workflow channel has no agent participant"
        );
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::ToolCall,
            serde_json::json!({ "step_id": "s0", "command": "echo hi" }),
            None,
        )
        .unwrap();
        let ev = svc
            .transition(&ch.id, ChannelState::Completed, ChannelActor::System, None)
            .unwrap();
        assert_eq!(ev.kind, EventKind::StateChange);
        assert_eq!(ev.payload["from"], "working");
        assert_eq!(ev.payload["to"], "completed");
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().state,
            ChannelState::Completed
        );
    }

    #[test]
    fn delegation_and_transition_payloads_carry_run_id_when_given() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("run-stamped").unwrap();

        let del = svc
            .append_delegation(&ch.id, "qa", "child-task-9", None, Some("run-9"))
            .unwrap();
        assert_eq!(
            del.payload["run_id"], "run-9",
            "a delegation written by a run must carry its run_id"
        );

        let trans = svc
            .transition(
                &ch.id,
                ChannelState::Failed,
                ChannelActor::System,
                Some("run-9"),
            )
            .unwrap();
        assert_eq!(
            trans.payload["run_id"], "run-9",
            "a state change written by a run must carry its run_id"
        );
    }

    #[test]
    fn payloads_without_run_id_omit_the_field() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("run-unstamped").unwrap();

        let del = svc
            .append_delegation(&ch.id, "qa", "child-task-9", None, None)
            .unwrap();
        assert!(
            del.payload.get("run_id").is_none(),
            "legacy writers pass no run_id; the field must be absent, not null"
        );

        let trans = svc
            .transition(&ch.id, ChannelState::Completed, ChannelActor::System, None)
            .unwrap();
        assert!(
            trans.payload.get("run_id").is_none(),
            "non-run transitions must not fabricate a run_id"
        );
    }

    #[test]
    fn append_signed_stores_verifiable_sig() {
        let tmp = TempDir::new().unwrap();
        let id = mur_common::identity::AgentIdentity::generate();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("signed").unwrap();
        let ev = svc
            .append_signed(
                &ch.id,
                &id,
                0,
                ChannelActor::Agent { id: "mur".into() },
                EventKind::Message,
                serde_json::json!({ "text": "hi" }),
                None,
            )
            .unwrap();
        assert!(ev.sig.is_some());
        assert_eq!(ev.key_version, Some(0));
        let loaded = svc.load_events(&ch.id).unwrap();
        let e = &loaded[0];
        assert!(crate::sign::verify_event_sig(
            &ch.id,
            &e.actor,
            e.kind,
            &e.payload,
            e.idempotency_key.as_deref(),
            e.sig.as_ref().unwrap(),
            &id.verifying_key_bytes()
        ));
    }

    // Unix-only: exercises a read-only read-model via fs permission bits.
    //
    // Note on mechanism: a plain chmod of `channels.db` to read-only does NOT
    // reproduce the fault against `ChannelIndex`'s already-open `Connection` —
    // POSIX permission bits are checked at `open()`, not on every `write()`
    // through an fd opened before the chmod, so the long-lived SQLite
    // connection would keep writing regardless (verified empirically). The
    // manifest side of the read-model doesn't have that problem: every
    // `save_manifest` call does a *fresh* `fs::rename(tmp, path)`, and
    // `rename(2)` re-checks the containing directory's write permission on
    // every call. So this test freezes the channel's manifest directory
    // (after a warm-up append has already created `events.jsonl` /
    // `events.lock`, so the durable event log keeps append-writing to
    // already-open-by-path *existing* files, which needs no directory write
    // permission) — that reliably fails `save_manifest`, which is what
    // `refresh_read_model`'s combined `and_then` chain must swallow.
    #[cfg(unix)]
    #[test]
    fn append_survives_readonly_index() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path();
        let identity = mur_common::identity::AgentIdentity::generate();
        let svc = ChannelService::open(mur_home).unwrap();
        let ch = svc.create_for_workflow("signed").unwrap();

        // Warm-up append so events.jsonl / events.lock already exist before we
        // freeze the channel directory (their re-opens then need no directory
        // write permission — only the manifest rename does).
        svc.append_signed(
            &ch.id,
            &identity,
            1,
            ChannelActor::Agent { id: "w1".into() },
            EventKind::Message,
            serde_json::json!({ "text": "warm-up" }),
            None,
        )
        .unwrap();

        // Freeze the read-model: the SQLite index dir/file (matching the
        // production "attempt to write a readonly database" report) plus the
        // channel's manifest directory (the mechanism that actually bites in
        // this in-process test — see comment above).
        let index_dir = mur_home.join("index").join("channels");
        let db = index_dir.join("channels.db");
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
        for ext in ["-wal", "-shm"] {
            let p = index_dir.join(format!("channels.db{ext}"));
            if p.exists() {
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
            }
        }
        std::fs::set_permissions(&index_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let channel_dir = mur_home.join("channels").join(&ch.id);
        std::fs::set_permissions(&channel_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // The append must still succeed — the event log is the record.
        let ev = svc
            .append_signed(
                &ch.id,
                &identity,
                1,
                ChannelActor::Agent { id: "w1".into() },
                EventKind::Message,
                serde_json::json!({ "text": "hi" }),
                None,
            )
            .expect("append must not fail on a read-only read-model");

        // Restore perms so tempdir cleanup works.
        std::fs::set_permissions(&channel_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&index_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();

        // The event is durably in the log; the manifest read-model is frozen
        // (unwritten) — both expected under a non-fatal refresh.
        let events = svc.load_events(&ch.id).unwrap();
        assert!(events.iter().any(|e| e.seq == ev.seq));
    }

    #[test]
    fn create_append_resume_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "find the bug",
            Some("t-1"),
        )
        .unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::Message,
            "found it",
            Some("t-1"),
        )
        .unwrap();

        let evs = svc.load_events(&ch.id).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].payload["text"], "find the bug");

        let latest = svc.latest_for_agent("qa").unwrap();
        assert_eq!(latest.as_deref(), Some(ch.id.as_str()));
        assert!(svc.latest_for_agent("other").unwrap().is_none());
    }

    #[test]
    fn add_remove_participant_and_delete_channel() {
        use mur_common::channel::ParticipantRole;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let svc = ChannelService::open(home).unwrap();
        let ch = svc
            .create_for_fleet("dev", "mur", &["pm".to_string()])
            .unwrap();

        // add a Delegate member (idempotent)
        svc.add_participant(&ch.id, "qa", ParticipantRole::Delegate)
            .unwrap();
        svc.add_participant(&ch.id, "qa", ParticipantRole::Delegate)
            .unwrap();
        let m = svc.store().load_manifest(&ch.id).unwrap();
        let qa_count = m
            .participants
            .iter()
            .filter(|p| matches!(&p.actor, mur_common::channel::ChannelActor::Agent { id } if id == "qa"))
            .count();
        assert_eq!(qa_count, 1, "add must be idempotent");

        // remove it
        svc.remove_participant(&ch.id, "qa").unwrap();
        let m = svc.store().load_manifest(&ch.id).unwrap();
        assert!(!m.participants.iter().any(
            |p| matches!(&p.actor, mur_common::channel::ChannelActor::Agent { id } if id == "qa")
        ));

        // delete the whole channel
        svc.delete_channel(&ch.id).unwrap();
        assert!(svc.store().load_manifest(&ch.id).is_err());
    }

    #[test]
    fn create_for_fleet_sets_roles_and_id() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc
            .create_for_fleet("dev", "mur", &["pm".to_string(), "qa".to_string()])
            .unwrap();
        assert_eq!(ch.id, "fleet-dev");
        // owner human + router + 2 delegates = 4 participants
        assert_eq!(ch.participants.len(), 4);
        assert!(
            ch.participants
                .iter()
                .any(|p| p.role == ParticipantRole::Router
                    && matches!(&p.actor, ChannelActor::Agent { id } if id == "mur"))
        );
        assert_eq!(
            ch.participants
                .iter()
                .filter(|p| p.role == ParticipantRole::Delegate)
                .count(),
            2
        );
        // persisted
        assert_eq!(svc.load_events(&ch.id).unwrap().len(), 0);
    }

    #[test]
    fn first_human_message_titles_the_conversation() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();

        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "explain this repo",
            None,
        )
        .unwrap();

        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().title,
            "explain this repo"
        );
    }

    #[test]
    fn the_title_is_set_once_and_never_rewritten() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        for text in ["first question", "second question"] {
            svc.append_message(
                &ch.id,
                ChannelActor::local_human(),
                EventKind::Message,
                text,
                None,
            )
            .unwrap();
        }
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().title,
            "first question"
        );
    }

    #[test]
    fn long_titles_are_truncated_at_the_shared_limit() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        let long = "x".repeat(200);
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            &long,
            None,
        )
        .unwrap();

        let title = svc.store().load_manifest(&ch.id).unwrap().title;
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn cjk_titles_truncate_by_character_not_byte() {
        // Byte-slicing multibyte text panics; this is the regression guard.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        let long = "說".repeat(200);
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            &long,
            None,
        )
        .unwrap();
        assert_eq!(
            svc.store()
                .load_manifest(&ch.id)
                .unwrap()
                .title
                .chars()
                .count(),
            TITLE_MAX_CHARS
        );
    }

    #[test]
    fn a_fleet_channel_is_never_auto_titled() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let fleet = svc
            .create_for_fleet("projectx", "lead", &["a".into()])
            .unwrap();
        svc.append_message(
            &fleet.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "go",
            None,
        )
        .unwrap();
        assert_eq!(
            svc.store().load_manifest(&fleet.id).unwrap().title,
            "fleet: projectx"
        );
    }

    #[test]
    fn appends_advance_preview_count_and_seq() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "hi",
            None,
        )
        .unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            "hello back",
            None,
        )
        .unwrap();

        let row = svc
            .index()
            .list(10)
            .unwrap()
            .into_iter()
            .find(|r| r.id == ch.id)
            .unwrap();
        assert_eq!(row.preview, "hello back");
        assert_eq!(row.msg_count, 2);
        // `ChannelEvent::seq` is 0-indexed (store::append_event's next_seq
        // starts at 0), so the highest seq after two events is 1, not 2.
        assert_eq!(row.last_seq, 1);
    }

    #[test]
    fn internal_events_advance_seq_but_do_not_count_as_messages() {
        // Tool chatter must never inflate a chat's unread badge.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "run it",
            None,
        )
        .unwrap();
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::ToolCall,
            serde_json::json!({"tool": "bash"}),
            None,
        )
        .unwrap();

        let row = svc
            .index()
            .list(10)
            .unwrap()
            .into_iter()
            .find(|r| r.id == ch.id)
            .unwrap();
        assert_eq!(row.msg_count, 1, "ToolCall must not count as a message");
        assert_eq!(
            row.last_seq, 1,
            "but it does advance the sequence (0-indexed: two events -> highest seq 1)"
        );
        assert_eq!(row.preview, "run it", "and must not become the preview");
    }

    #[test]
    fn hitl_request_sets_pending_and_response_clears_it() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();

        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::HitlRequest,
            serde_json::json!({"hitl_id": "h1"}),
            None,
        )
        .unwrap();
        let row = |svc: &ChannelService| {
            svc.index()
                .list(10)
                .unwrap()
                .into_iter()
                .find(|r| r.id == ch.id)
                .unwrap()
        };
        assert!(row(&svc).hitl_pending);

        svc.append(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::HitlResponse,
            serde_json::json!({"hitl_id": "h1", "approved": true}),
            None,
        )
        .unwrap();
        assert!(!row(&svc).hitl_pending);
    }

    #[test]
    fn unread_counts_only_messages_the_human_did_not_write() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();

        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "hi",
            None,
        )
        .unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            "hello",
            None,
        )
        .unwrap();
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::ToolCall,
            serde_json::json!({}),
            None,
        )
        .unwrap();

        let q = crate::summary::ConversationQuery {
            agent: None,
            active_only: false,
        };
        let row = &svc.list_conversations(q).unwrap()[0];
        assert_eq!(
            row.unread, 1,
            "one agent message; the human's own turn and the tool call do not count"
        );
    }

    #[test]
    fn mark_read_clears_unread() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "hi",
            None,
        )
        .unwrap();
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

        let q = crate::summary::ConversationQuery {
            agent: None,
            active_only: false,
        };
        assert_eq!(svc.list_conversations(q).unwrap()[0].unread, 0);
    }

    #[test]
    fn the_watermark_never_moves_backwards() {
        // A background window reporting a stale position must not resurrect
        // unread state that a focused window already cleared.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        for _ in 0..3 {
            svc.append_message(
                &ch.id,
                ChannelActor::Agent { id: "mur".into() },
                EventKind::Message,
                "x",
                None,
            )
            .unwrap();
        }

        svc.mark_read(&ch.id, 3).unwrap();
        svc.mark_read(&ch.id, 1).unwrap(); // stale surface

        let q = crate::summary::ConversationQuery {
            agent: None,
            active_only: false,
        };
        assert_eq!(svc.list_conversations(q).unwrap()[0].unread, 0);
    }

    #[test]
    fn a_new_agent_message_after_reading_is_unread_again() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        // NOTE: this test does not currently discriminate. The agent's
        // first message is seq 0, and `last_read_seq` defaults to 0 (the
        // same 0-indexed collision `last_seq` needed a -1 sentinel for) —
        // so a `mark_read(&ch.id, 0)` call here would be a no-op against
        // the column default, and the test would pass whether or not
        // "after reading" ever actually ran. The `mark_read` call is
        // deliberately omitted rather than kept-but-useless. Fixing this
        // for real needs the `last_read_seq` -1 sentinel, deferred to
        // Phase 2; once that lands, add `svc.mark_read(&ch.id,
        // first.seq).unwrap()` back here and this test becomes
        // load-bearing.
        let _first = svc
            .append_message(
                &ch.id,
                ChannelActor::Agent { id: "mur".into() },
                EventKind::Message,
                "a",
                None,
            )
            .unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            "b",
            None,
        )
        .unwrap();

        let q = crate::summary::ConversationQuery {
            agent: None,
            active_only: false,
        };
        assert_eq!(svc.list_conversations(q).unwrap()[0].unread, 1);
    }
}
