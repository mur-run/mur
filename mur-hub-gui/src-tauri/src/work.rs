//! Read-only channel queries for the Hub "Work" view (Unified Channel v2).
//!
//! The Work view is observability over the shared `~/.mur/channels/` store: it
//! lists every channel, shows one channel's event stream, and surfaces goal /
//! participants / state. It writes nothing — two-way chat stays in `chat.rs`.
//!
//! Command logic lives in pure helpers taking `home: &Path` so it is unit-tested
//! against a tempdir (mirrors `chat::persist_exchange`); the `#[tauri::command]`
//! wrappers are thin shims that pass `mur_home_path()`.

use mur_channel::ChannelService;
use mur_common::channel::{Channel, ChannelActor, ChannelEvent};
use serde::Serialize;
use std::path::Path;

/// A channel participant flattened for the frontend.
#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct WorkParticipant {
    /// "human" | "agent" | "system".
    pub kind: String,
    /// Agent id or human name ("" for system).
    pub id: String,
    /// "owner" | "router" | "delegate" | "observer".
    pub role: String,
}

/// One row in the Work left rail. Folds the manifest + a cheap event scan.
#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct ChannelSummary {
    pub id: String,
    pub title: String,
    /// kebab-case `ChannelState`.
    pub state: String,
    /// `goal.statement` (may be empty).
    pub goal: String,
    pub created_at: String,
    pub updated_at: String,
    pub participants: Vec<WorkParticipant>,
    /// Convenience: just the agent participant ids, for the rail's avatars.
    pub agents: Vec<String>,
    /// Event count (turns).
    pub turns: usize,
    /// First human message, truncated — the rail's subtitle.
    pub preview: String,
}

/// Serialize a `ChannelState`/role/actor enum to its kebab/lowercase string.
fn enum_str<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

fn participant_of(actor: &ChannelActor, role_str: String) -> WorkParticipant {
    let (kind, id) = match actor {
        ChannelActor::Human { name } => ("human", name.clone()),
        ChannelActor::Agent { id } => ("agent", id.clone()),
        ChannelActor::System => ("system", String::new()),
    };
    WorkParticipant {
        kind: kind.to_string(),
        id,
        role: role_str,
    }
}

/// Pure: build a rail summary from a manifest + its events. No I/O.
pub fn summary_of(ch: &Channel, events: &[ChannelEvent]) -> ChannelSummary {
    let participants: Vec<WorkParticipant> = ch
        .participants
        .iter()
        .map(|p| participant_of(&p.actor, enum_str(&p.role)))
        .collect();
    let agents: Vec<String> = participants
        .iter()
        .filter(|p| p.kind == "agent")
        .map(|p| p.id.clone())
        .collect();
    let preview = events
        .iter()
        .find(|e| matches!(e.actor, ChannelActor::Human { .. }))
        .and_then(|e| e.payload.get("text").and_then(|v| v.as_str()))
        .unwrap_or("")
        .chars()
        .take(80)
        .collect();
    ChannelSummary {
        id: ch.id.clone(),
        title: ch.title.clone(),
        state: enum_str(&ch.state),
        goal: ch.goal.statement.clone(),
        created_at: ch.created_at.to_rfc3339(),
        updated_at: ch.updated_at.to_rfc3339(),
        participants,
        agents,
        turns: events.len(),
        preview,
    }
}

/// Max channels surfaced in the rail. v2 scale is small; bump when the index
/// grows a participant column (v1-accepted follow-up).
const WORK_LIST_LIMIT: usize = 200;

/// List channels newest-first for the Work rail. Folds each manifest with a
/// cheap event scan; empty channels (created-but-never-written stubs) are
/// hidden so the rail never shows blank rows.
pub fn list_channels(home: &Path) -> anyhow::Result<Vec<ChannelSummary>> {
    let svc = ChannelService::open(home)?;
    let mut out = Vec::new();
    for row in svc.list(WORK_LIST_LIMIT)? {
        let events = svc.load_events(&row.id).unwrap_or_default();
        if events.is_empty() {
            continue;
        }
        let Ok(manifest) = svc.store().load_manifest(&row.id) else {
            continue;
        };
        out.push(summary_of(&manifest, &events));
    }
    Ok(out)
}

/// All events for one channel (the feed).
pub fn events_for(home: &Path, id: &str) -> anyhow::Result<Vec<ChannelEvent>> {
    let svc = ChannelService::open(home)?;
    svc.load_events(id)
}

/// One channel manifest (the trace pane: goal / participants / state).
pub fn manifest_for(home: &Path, id: &str) -> anyhow::Result<Channel> {
    let svc = ChannelService::open(home)?;
    svc.store().load_manifest(id)
}

/// Tauri: list all channels for the Work rail.
#[tauri::command]
pub async fn channel_list() -> Result<Vec<ChannelSummary>, String> {
    let home = crate::mur_home_path();
    tokio::task::spawn_blocking(move || list_channels(&home))
        .await
        .map_err(|e| format!("channel_list task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::EventKind;
    use tempfile::TempDir;

    #[test]
    fn summary_of_extracts_agents_preview_and_state() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "find the bug",
            None,
        )
        .unwrap();
        let manifest = svc.store().load_manifest(&ch.id).unwrap();
        let events = svc.load_events(&ch.id).unwrap();

        let s = summary_of(&manifest, &events);
        assert_eq!(s.id, ch.id);
        assert_eq!(s.agents, vec!["qa".to_string()]);
        assert_eq!(s.preview, "find the bug");
        assert_eq!(s.turns, 1);
        // v1 freezes state at its initial value; just assert it round-trips kebab.
        assert!(!s.state.is_empty() && !s.state.contains('"'));
    }

    #[test]
    fn list_channels_folds_manifests_and_skips_empty() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        // One real channel with a turn…
        let a = svc.create_for_agent("qa").unwrap();
        svc.append_message(
            &a.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "hi",
            None,
        )
        .unwrap();
        // …and one empty stub that must be filtered out of the rail.
        let _empty = svc.create_for_agent("ghost").unwrap();

        let rows = list_channels(tmp.path()).unwrap();
        assert_eq!(rows.len(), 1, "empty channels are hidden from the rail");
        assert_eq!(rows[0].id, a.id);
        assert_eq!(rows[0].agents, vec!["qa".to_string()]);

        // events_for + manifest_for hit the same channel.
        let evs = events_for(tmp.path(), &a.id).unwrap();
        assert_eq!(evs.len(), 1);
        let m = manifest_for(tmp.path(), &a.id).unwrap();
        assert_eq!(m.id, a.id);
    }
}
