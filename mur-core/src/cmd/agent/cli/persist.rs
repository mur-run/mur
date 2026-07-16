//! CLI transcript persistence, backed by the unified Channel store.
//! The public surface (`Session`, `TurnRecord`, `SessionInfo`, `list_recent`,
//! `load`, `latest`) is preserved so `app.rs`/`mod.rs` are barely touched.
//!
//! v3d: turns are SIGNED by the session's agent (the channel's writer) via
//! `crate::channel_writer::append_as_writer`, falling back to unsigned when the
//! agent has no on-disk identity (migration-safe).
use std::path::{Path, PathBuf};

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

/// Live-channel id + state for the CLI status bar.
#[derive(Debug, Clone)]
pub struct ChannelMeta {
    pub id: String,
    /// kebab-case `ChannelState` string (e.g. "working").
    pub state: String,
}

/// Listing entry; `id` is the channel id (was the session file stem).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub preview: String,
    pub turns: usize,
}

/// A live conversation handle. The backing channel is created lazily on the
/// first append, so launching the TUI (or `/clear`) and typing nothing leaves no
/// empty channel on disk — and `--resume` never resurfaces a blank conversation.
pub struct Session {
    svc: ChannelService,
    /// `None` until the first append creates the channel.
    channel_id: Option<String>,
    agent: String,
    /// `~/.mur` root — needed to resolve the agent's signing identity (v3d).
    home: PathBuf,
}

impl Session {
    /// Prepare a fresh session. No channel is written until the first append.
    pub fn create(home: &Path, agent: &str) -> Result<Self> {
        let svc = ChannelService::open(home)?;
        Ok(Self {
            svc,
            channel_id: None,
            agent: agent.to_string(),
            home: home.to_path_buf(),
        })
    }

    /// Re-open an existing channel by id (used by `--resume`).
    pub fn open_existing(home: &Path, agent: &str, channel_id: &str) -> Result<Self> {
        let svc = ChannelService::open(home)?;
        Ok(Self {
            svc,
            channel_id: Some(channel_id.to_string()),
            agent: agent.to_string(),
            home: home.to_path_buf(),
        })
    }

    // Part of the preserved public surface; consumed by the integration tests,
    // so it reads as dead within the bin crate. `None` before the first append.
    #[allow(dead_code)]
    pub fn channel_id(&self) -> Option<&str> {
        self.channel_id.as_deref()
    }

    /// Read the live channel's id + state for the status bar.
    /// Returns `None` until the first `append` creates the channel.
    pub fn current(&self) -> Option<ChannelMeta> {
        let id = self.channel_id.clone()?;
        let ch = self.svc.store().load_manifest(&id).ok()?;
        let state = serde_json::to_string(&ch.state)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        Some(ChannelMeta { id, state })
    }

    /// Append one turn, creating the channel on first write. `role` ∈
    /// {"user","agent","shell"}. For agent turns, `suggested` carries the
    /// quick-reply options offered alongside the reply (#716): they are
    /// persisted into the event payload as an additive `suggested_replies`
    /// field so channel history is not lossy about what was offered.
    pub fn append(
        &mut self,
        role: &str,
        text: &str,
        task_id: Option<&str>,
        suggested: &[super::suggest::Suggestion],
    ) -> Result<()> {
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
        let id = match &self.channel_id {
            Some(id) => id.clone(),
            None => {
                let ch = self.svc.create_for_agent(&self.agent)?;
                self.channel_id = Some(ch.id.clone());
                ch.id
            }
        };
        // Same payload shape as `append_message`, but SIGNED by the session's
        // agent (the channel writer) when an identity exists (v3d).
        let mut payload = serde_json::json!({ "text": text });
        if let Some(t) = task_id {
            payload["task_id"] = serde_json::Value::String(t.to_string());
        }
        // Additive field only — absent when no options were offered, so
        // existing events and readers are untouched (new events sign whatever
        // payload they carry; no fold/verify change).
        if role == "agent" && !suggested.is_empty() {
            payload["suggested_replies"] = suggestions_to_json(suggested);
        }
        crate::channel_writer::append_as_writer(
            &self.svc,
            &self.home,
            &id,
            &self.agent,
            actor,
            kind,
            payload,
            None,
        )?;
        Ok(())
    }
}

/// Serialize offered quick-reply options for the channel payload: a bare
/// string when there is no description, else `{text, description}` —
/// mirroring the tool-call shape so nothing offered is lost.
fn suggestions_to_json(suggested: &[super::suggest::Suggestion]) -> serde_json::Value {
    serde_json::Value::Array(
        suggested
            .iter()
            .map(|s| match &s.desc {
                Some(d) => serde_json::json!({ "text": s.text, "description": d }),
                None => serde_json::Value::String(s.text.clone()),
            })
            .collect(),
    )
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
        // Skip empty channels (e.g. legacy stubs from before lazy creation) so
        // `--resume`/`/sessions` never surface a blank conversation.
        if evs.is_empty() {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn current_is_none_until_first_append_then_some() {
        let tmp = TempDir::new().unwrap();
        let mut s = Session::create(tmp.path(), "qa").unwrap();
        assert!(s.current().is_none(), "no channel before first append");
        s.append("user", "hi", None, &[]).unwrap();
        let meta = s.current().expect("channel exists after first append");
        assert!(!meta.id.is_empty());
        assert!(!meta.state.is_empty());
        // state must not contain JSON quotes — must be bare kebab
        assert!(!meta.state.contains('"'));
    }

    /// #716: options offered via `suggest_replies` must land in the agent
    /// event's payload (`suggested_replies`) so channel history is not lossy;
    /// turns without options must not carry the field.
    #[test]
    fn agent_append_persists_offered_suggestions() {
        let tmp = TempDir::new().unwrap();
        let mut s = Session::create(tmp.path(), "qa").unwrap();
        s.append("user", "run the fleet?", None, &[]).unwrap();
        let offered = vec![
            crate::cmd::agent::cli::suggest::Suggestion {
                text: "取消".into(),
                desc: None,
            },
            crate::cmd::agent::cli::suggest::Suggestion {
                text: "還是執行".into(),
                desc: Some("硬跑".into()),
            },
        ];
        s.append(
            "agent",
            "fleet 目標不符：Rust fleet、Next.js job。要取消還是硬跑？",
            Some("task-1"),
            &offered,
        )
        .unwrap();
        s.append("agent", "no options this turn", None, &[])
            .unwrap();

        let id = s.channel_id().unwrap().to_string();
        let evs = ChannelService::open(tmp.path())
            .unwrap()
            .load_events(&id)
            .unwrap();
        let agent_evs: Vec<_> = evs
            .iter()
            .filter(|e| matches!(e.actor, ChannelActor::Agent { .. }))
            .collect();
        assert_eq!(agent_evs.len(), 2);

        let sugg = agent_evs[0]
            .payload
            .get("suggested_replies")
            .expect("agent event carries the offered options");
        assert_eq!(sugg[0], serde_json::json!("取消"));
        assert_eq!(
            sugg[1],
            serde_json::json!({ "text": "還是執行", "description": "硬跑" })
        );
        // Existing payload fields are untouched.
        assert_eq!(agent_evs[0].payload["task_id"], "task-1");
        assert!(
            agent_evs[0].payload["text"]
                .as_str()
                .unwrap()
                .contains("目標不符")
        );
        // No options offered → field absent (additive, never an empty array).
        assert!(agent_evs[1].payload.get("suggested_replies").is_none());
    }
}
