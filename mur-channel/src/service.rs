use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use mur_common::channel::{
    CHANNEL_SCHEMA_VERSION, Channel, ChannelActor, ChannelEvent, ChannelState, EventKind, Goal,
    Participant, ParticipantRole,
};

use crate::index::{ChannelIndex, ChannelRow};
use crate::store::ChannelStore;

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
        Ok(Self {
            store: ChannelStore::new(mur_home),
            index: ChannelIndex::open(mur_home)?,
        })
    }

    /// Create a fresh channel whose participants are the local human (owner)
    /// and one agent (delegate). Used by both CLI and Hub when opening a chat.
    pub fn create_for_agent(&self, agent: &str) -> Result<Channel> {
        let now = Utc::now();
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: uuid::Uuid::now_v7().to_string(),
            title: format!("chat with {agent}"),
            goal: Goal::default(),
            state: ChannelState::Working,
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
            .append_event(channel_id, actor, kind, payload, None)?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
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
        let ev = self
            .store
            .append_event(channel_id, actor, kind, payload, idempotency_key)?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(ev)
    }

    /// Emit a `StateChange` event and persist the new state on the manifest.
    pub fn transition(
        &self,
        channel_id: &str,
        new_state: ChannelState,
        actor: ChannelActor,
    ) -> Result<ChannelEvent> {
        let old_state = self
            .store
            .load_manifest(channel_id)
            .map(|ch| ch.state)
            .unwrap_or(ChannelState::Working);
        let payload = serde_json::json!({
            "from": state_str(old_state),
            "to":   state_str(new_state),
        });
        let ev = self
            .store
            .append_event(channel_id, actor, EventKind::StateChange, payload, None)?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.state = new_state;
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
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

    pub fn store(&self) -> &ChannelStore {
        &self.store
    }
    pub fn index(&self) -> &ChannelIndex {
        &self.index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_structured_and_transition() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("deploy").unwrap();
        assert_eq!(ch.state, ChannelState::Working);
        assert!(ch.participants.is_empty(), "workflow channel has no agent participant");
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::ToolCall,
            serde_json::json!({ "step_id": "s0", "command": "echo hi" }),
            None,
        )
        .unwrap();
        let ev = svc
            .transition(&ch.id, ChannelState::Completed, ChannelActor::System)
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
}
