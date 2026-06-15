//! CLI transcript persistence, backed by the unified Channel store.
//! The public surface (`Session`, `TurnRecord`, `SessionInfo`, `list_recent`,
//! `load`, `latest`) is preserved so `app.rs`/`mod.rs` are barely touched.
use std::path::Path;

use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use serde::{Deserialize, Serialize};

/// In-memory shape consumed by `App::load_history`. (No longer a JSONL line.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub ts: String,
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// Listing entry; `id` is the channel id (was the session file stem).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub preview: String,
    pub turns: usize,
}

/// A live conversation handle bound to one channel.
pub struct Session {
    svc: ChannelService,
    channel_id: String,
    agent: String,
}

impl Session {
    /// Open a fresh channel for `agent`.
    pub fn create(home: &Path, agent: &str) -> Result<Self> {
        let svc = ChannelService::open(home)?;
        let ch = svc.create_for_agent(agent)?;
        Ok(Self {
            svc,
            channel_id: ch.id,
            agent: agent.to_string(),
        })
    }

    /// Re-open an existing channel by id (used by `--resume`).
    pub fn open_existing(home: &Path, agent: &str, channel_id: &str) -> Result<Self> {
        let svc = ChannelService::open(home)?;
        Ok(Self {
            svc,
            channel_id: channel_id.to_string(),
            agent: agent.to_string(),
        })
    }

    // Part of the preserved public surface; consumed by the integration test
    // (`tests/cli_channel_persist.rs`), so it reads as dead within the bin crate.
    #[allow(dead_code)]
    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    /// Append one turn. `role` ∈ {"user","agent","shell"}.
    pub fn append(&self, role: &str, text: &str, task_id: Option<&str>) -> Result<()> {
        let (actor, kind) = match role {
            "agent" => (
                ChannelActor::Agent {
                    id: self.agent.clone(),
                },
                EventKind::Message,
            ),
            "shell" => (ChannelActor::System, EventKind::Note),
            _ => (ChannelActor::local_human(), EventKind::Message),
        };
        self.svc
            .append_message(&self.channel_id, actor, kind, text, task_id)?;
        Ok(())
    }
}

fn event_to_turn(ev: &ChannelEvent) -> TurnRecord {
    let text = ev
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let task_id = ev
        .payload
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let role = match &ev.actor {
        ChannelActor::Agent { .. } => "agent",
        ChannelActor::System => "shell",
        ChannelActor::Human { .. } => "user",
    }
    .to_string();
    TurnRecord {
        ts: ev.ts.to_rfc3339(),
        role,
        text,
        task_id,
    }
}

/// Load a channel's turns for `App::load_history`.
pub fn load(home: &Path, channel_id: &str, _agent: &str) -> Result<Vec<TurnRecord>> {
    let svc = ChannelService::open(home)?;
    Ok(svc
        .load_events(channel_id)?
        .iter()
        .map(event_to_turn)
        .collect())
}

/// Newest channels that involve `agent`, newest-first.
pub fn list_recent(home: &Path, agent: &str, limit: usize) -> Result<Vec<SessionInfo>> {
    let svc = ChannelService::open(home)?;
    let mut out = Vec::new();
    for row in svc.list(1000)? {
        let involved = svc
            .store()
            .load_manifest(&row.id)
            .map(|ch| {
                ch.participants
                    .iter()
                    .any(|p| matches!(&p.actor, ChannelActor::Agent { id } if id == agent))
            })
            .unwrap_or(false);
        if !involved {
            continue;
        }
        let evs = svc.load_events(&row.id)?;
        let preview = evs
            .iter()
            .find(|e| matches!(e.actor, ChannelActor::Human { .. }))
            .and_then(|e| e.payload.get("text").and_then(|v| v.as_str()))
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        out.push(SessionInfo {
            id: row.id,
            preview,
            turns: evs.len(),
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

pub fn latest(home: &Path, agent: &str) -> Result<Option<SessionInfo>> {
    Ok(list_recent(home, agent, 1)?.into_iter().next())
}
