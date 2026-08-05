use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use mur_common::channel::{
    CHANNEL_SCHEMA_VERSION, Channel, ChannelActor, ChannelEvent, ChannelState, EventKind, Goal,
    Participant, ParticipantRole,
};

use crate::index::{ChannelIndex, ChannelRow};
use crate::store::ChannelStore;

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
) -> serde_json::Value {
    serde_json::to_value(DelegationPayload {
        target_agent,
        child_task_id,
        parent_channel_id,
        goal,
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
            self.refresh_read_model(&ch);
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
            self.refresh_read_model(&ch);
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
            self.refresh_read_model(&ch);
        }
        Ok(ev)
    }

    /// Append a `Delegation` event (actor `System`) recording that `target_agent`
    /// was handed the sub-goal under `child_task_id`. `idempotency_key` is set by
    /// the caller (deterministic in v3b) but NOT yet de-duplicated (v3c).
    pub fn append_delegation(
        &self,
        channel_id: &str,
        target_agent: &str,
        child_task_id: &str,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let payload = delegation_payload(channel_id, target_agent, child_task_id, None);
        self.append(
            channel_id,
            ChannelActor::System,
            EventKind::Delegation,
            payload,
            idempotency_key,
        )
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
            self.refresh_read_model(&ch);
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

    /// Refresh the manifest + SQLite read-model after a successful event
    /// append. Both are rebuildable projections of `events.jsonl` — a
    /// refresh failure must not fail an append whose event is already
    /// durable. Concretely: a sandboxed delegate (peer-writes-own, v3d-2)
    /// may be able to write the channel store but not the shared index;
    /// SQLite reports the denied write as "attempt to write a readonly
    /// database" (G3, live fleet run 2026-07-09).
    fn refresh_read_model(&self, ch: &Channel) {
        if let Err(e) = self
            .store
            .save_manifest(ch)
            .and_then(|()| self.index.upsert(ch))
        {
            tracing::warn!(
                channel_id = %ch.id,
                error = %e,
                "read-model refresh failed after append (event persisted; index is rebuildable)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_delegation_writes_typed_event() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("delegating-wf").unwrap();
        let ev = svc
            .append_delegation(&ch.id, "qa", "child-task-1", Some("idem-1".into()))
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
}
