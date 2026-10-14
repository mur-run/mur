use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use mur_common::channel::{
    CHANNEL_SCHEMA_VERSION, Channel, ChannelActor, ChannelEvent, ChannelPurpose, ChannelState,
    EventKind, Goal, Participant, ParticipantRole,
};

use crate::index::{ChannelIndex, ChannelRow};
use crate::store::{ChannelStore, SealInput};

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

/// An owned, shareable sealing hook for [`ChannelService`] (spec 7.3.1). The
/// store takes a borrowed [`SealHook`]; the service keeps this `Arc` and lends
/// it per write.
pub type SharedSealHook = Arc<dyn for<'a, 'b> Fn(&'b SealInput<'a>) -> Result<()> + Send + Sync>;

pub struct ChannelService {
    store: ChannelStore,
    index: ChannelIndex,
    /// Run after every event line this service writes. `None` (the default)
    /// leaves behaviour exactly as before sealing existed.
    seal: Option<SharedSealHook>,
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
        Ok(Self {
            store,
            index,
            seal: None,
        })
    }

    /// Install the sealing hook run after every event line this service
    /// writes (spec 7.3.1). Without it the service behaves as before.
    #[must_use]
    pub fn with_seal(mut self, seal: SharedSealHook) -> Self {
        self.seal = Some(seal);
        self
    }

    /// The one place the service writes an event: append through the store
    /// (lending it the seal hook), then bump the manifest's `updated_at`,
    /// derived title and, for a transition, `state`, and refresh the read
    /// model. A failed seal returns `Err` before the refresh; the line stays
    /// on disk (decision B1). A per-call `seal` wins over the service's own.
    #[allow(clippy::too_many_arguments)]
    fn write_event(
        &self,
        channel_id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        sig: Option<String>,
        key_version: Option<u32>,
        new_state: Option<ChannelState>,
        seal: Option<&crate::store::SealHook<'_>>,
    ) -> Result<ChannelEvent> {
        let ev = self.store.append_event_with(
            channel_id,
            actor,
            kind,
            payload,
            idempotency_key,
            sig,
            key_version,
            seal.or_else(|| {
                self.seal
                    .as_deref()
                    .map(|h| h as &crate::store::SealHook<'_>)
            }),
        )?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            if let Some(state) = new_state {
                ch.state = state;
            }
            // Monotonic: a dedup hit hands back the ORIGINAL event, whose `ts`
            // predates any activity since. Replaying it must not rewind recency.
            ch.updated_at = ch.updated_at.max(ev.ts);
            if let Some(t) = Self::derived_title(&ch, &ev) {
                ch.title = t;
            }
            self.refresh_read_model(&ch, &ev);
        }
        Ok(ev)
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
        self.write_event(
            channel_id, actor, kind, payload, None, None, None, None, None,
        )
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

    pub fn load_events(&self, channel_id: &str) -> Result<Vec<ChannelEvent>> {
        self.store.load_events(channel_id)
    }

    /// Whether a channel exists, judged by its manifest.
    ///
    /// `load_events` is not an existence check: a missing event log reads as an
    /// empty one, so it returns `Ok(vec![])` for a channel that was never
    /// created. Callers holding a remembered channel id need this instead.
    pub fn exists(&self, channel_id: &str) -> bool {
        self.store.load_manifest(channel_id).is_ok()
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

mod append;

#[cfg(test)]
mod tests;
