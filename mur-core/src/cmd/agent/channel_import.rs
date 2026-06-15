//! One-shot importer: turn legacy `~/.mur/agents/<agent>/cli-sessions/*.jsonl`
//! transcripts into Channels. Idempotent per-session via a `.imported` marker.
//! Reads the legacy JSONL directly (does NOT use cli::persist, which is being
//! migrated onto the channel store).
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use serde::Deserialize;

/// Minimal mirror of a legacy CLI `TurnRecord` JSONL line.
#[derive(Debug, Deserialize)]
struct LegacyTurn {
    #[serde(default)]
    role: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    task_id: Option<String>,
}

const SESSIONS_SUBDIR: &str = "cli-sessions";

/// Import every CLI session of every agent under `mur_home`. Returns the number
/// of channels created. Sessions already imported (marker present) are skipped.
pub fn migrate_all(mur_home: &Path) -> Result<usize> {
    let svc = ChannelService::open(mur_home)?;
    let agents_dir = mur_home.join("agents");
    if !agents_dir.exists() {
        return Ok(0);
    }
    let mut created = 0;
    for entry in fs::read_dir(&agents_dir)
        .with_context(|| format!("read {}", agents_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let agent = entry.file_name().to_string_lossy().to_string();
        let sessions = entry.path().join(SESSIONS_SUBDIR);
        if !sessions.exists() {
            continue;
        }
        for sess in fs::read_dir(&sessions)
            .with_context(|| format!("read {}", sessions.display()))?
        {
            let sess = sess?;
            let path = sess.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let marker = path.with_extension("imported");
            if marker.exists() {
                continue;
            }
            let content = fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let turns: Vec<LegacyTurn> = content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str::<LegacyTurn>(l).ok())
                .collect();
            if turns.is_empty() {
                continue;
            }
            let ch = svc.create_for_agent(&agent)?;
            for t in &turns {
                let (actor, kind) = match t.role.as_str() {
                    "agent" => (ChannelActor::Agent { id: agent.clone() }, EventKind::Message),
                    "shell" => (ChannelActor::System, EventKind::Note),
                    _ => (ChannelActor::local_human(), EventKind::Message),
                };
                svc.append_message(&ch.id, actor, kind, &t.text, t.task_id.as_deref())?;
            }
            fs::write(&marker, ch.id.as_bytes()).ok();
            created += 1;
        }
    }
    Ok(created)
}
