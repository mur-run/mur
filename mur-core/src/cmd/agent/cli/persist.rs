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

/// Live-channel id for the CLI status bar.
///
/// Id only. The channel's persisted lifecycle state used to ride along here and
/// be rendered beside the live turn state, which is a second source for one
/// fact — and it lost, because it was refreshed at two call sites while the
/// turn state is refreshed every frame (#940).
#[derive(Debug, Clone)]
pub struct ChannelMeta {
    pub id: String,
    /// Stable per-channel number, handed out in creation order and never
    /// reused. `0` means "not numbered yet" — an index row older than the
    /// ordinals table that has not been backfilled.
    pub ordinal: u64,
    /// What names this channel by id at `/channels`; see [`handle_for`].
    pub handle: String,
}

/// Listing entry; `id` is the channel id (was the session file stem).
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub preview: String,
    pub turns: usize,
    /// Stable per-channel number; see `ChannelMeta::ordinal`.
    pub ordinal: u64,
    /// What names this channel by id at `/channels`; see [`handle_for`].
    pub handle: String,
}

/// What a user types at `/channels` to name channel `id` by id
/// ([`Handles::typeable`]), reading the index for its neighbours. Fail-soft:
/// an unreadable index gives the minimum-length handle, which is what every
/// handle was before #2538.
///
/// [`Handles::typeable`]: mur_channel::handle::Handles::typeable
pub fn handle_for(home: &Path, id: &str) -> String {
    ChannelService::open(home)
        .and_then(|svc| svc.index().handles())
        .unwrap_or_default()
        .typeable(id)
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

    /// Read the live channel's id for the status bar.
    /// Returns `None` until the first `append` creates the channel.
    pub fn current(&self) -> Option<ChannelMeta> {
        let id = self.channel_id.clone()?;
        let ordinal = self.svc.index().ordinal_of(&id).ok().flatten().unwrap_or(0);
        // Recomputed on every call, which is once a turn: a conversation
        // minted next to this one makes its handle longer, and the footer
        // must not keep showing the one that has become ambiguous.
        let handle = self.svc.index().handles().unwrap_or_default().typeable(&id);
        Some(ChannelMeta {
            id,
            ordinal,
            handle,
        })
    }

    /// Append one turn, creating the channel on first write. `role` ∈
    /// {"user","agent","shell"}. For agent turns, `suggested` carries the
    /// quick-reply options offered alongside the reply (#716): they are
    /// persisted into the event payload as an additive `suggested_replies`
    /// field so channel history is not lossy about what was offered.
    ///
    /// Returns the written event's `seq` — the `context.channel_seq` a turn
    /// answering it sends (at-mention C1).
    pub fn append(
        &mut self,
        role: &str,
        text: &str,
        task_id: Option<&str>,
        suggested: &[super::suggest::Suggestion],
    ) -> Result<u64> {
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
                let ch = self
                    .svc
                    .create_for_agent(&self.agent, mur_channel::service::ChannelOwner::Legacy)?;
                self.channel_id = Some(ch.id.clone());
                ch.id
            }
        };
        // Same payload shape as `append_message`, but SIGNED by the session's
        // agent (the channel writer) when an identity exists (v3d).
        // An image-only send has no caption, and the image itself is not kept
        // in history. Never save that as an empty user text: it is replayed on
        // every later turn and an empty text block 400s the provider, wedging
        // the channel. Save the same marker the converters use instead.
        let text = if role == "user" {
            mur_channel::conversation::normalize_user_text(text)
        } else {
            text
        };
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
        // C2: a typed turn is signed with the human key (router-sealed,
        // enrolling the owner on first write); the reply and the `!cmd`
        // card stay signed by the session agent.
        //
        // The `!cmd` card is the one write here whose signer is not its
        // actor's own, and the type makes it say so: it is a `System` note
        // under the session agent's key, which is what the conversation
        // watermark checks it against (`mur_channel::conversation::assess`,
        // S1). The per-actor verifier (`verify_event`) holds a `System` actor
        // to the control plane instead, so for any other agent it rejects
        // this line. `mur channel verify` reads the card under the signer's
        // key and names the signer in its report (#2508, #2519).
        use crate::channel_writer::{Authorship, Signer};
        let author = match actor {
            ChannelActor::Human { .. } => None,
            ChannelActor::Agent { .. } => Some(Authorship::agent(&self.agent)),
            ChannelActor::System => Some(Authorship::out_of_role(
                Signer::Agent(&self.agent),
                ChannelActor::System,
            )),
        };
        let ev = match author {
            None => crate::human_writer::append_as_human(
                &self.svc, &self.home, &id, kind, payload, None,
            )?,
            Some(author) => crate::channel_writer::append_as_writer(
                &self.svc, &self.home, &id, author, kind, payload, None,
            )?,
        };
        Ok(ev.seq)
    }

    /// Withdraw the user event at `target_seq` from the history the model
    /// reads (retract spec §4.4): one signed `Retract`, keyed so a retried
    /// append is a no-op. The original line stays on disk.
    pub fn retract(&self, channel_id: &str, target_seq: u64) -> Result<u64> {
        let payload = mur_common::channel::RetractPayload {
            target_seq,
            reason: mur_common::channel::RetractReason::Cancelled,
        };
        // Signed by the same human key as the turn it withdraws: a retract
        // is honoured only when its actor equals the target's.
        let ev = crate::human_writer::append_as_human(
            &self.svc,
            &self.home,
            channel_id,
            EventKind::Retract,
            serde_json::to_value(payload)?,
            Some(mur_common::channel::retract_idempotency_key(target_seq)),
        )?;
        Ok(ev.seq)
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

/// Load a channel's turns for `App::load_history`. A message a valid
/// `Retract` withdrew is skipped, and so is the `Retract` itself (same rule
/// the runtime projects with). A retract the runtime refused withdraws
/// nothing, and its refusal note replays as a `system` notice so the user
/// sees why the message is still there.
pub fn load(home: &Path, channel_id: &str, agent: &str) -> Result<Vec<TurnRecord>> {
    let svc = ChannelService::open(home)?;
    let events = svc.load_events(channel_id)?;
    let withdrawn = mur_channel::conversation::retract::scan(&events, agent, u64::MAX).targets;
    Ok(events
        .iter()
        // A human line that is not a `Message` (owner record, HITL answer,
        // retract) is control, never a user turn (C7).
        .filter(|e| !matches!(e.actor, ChannelActor::Human { .. }) || e.is_human_message())
        .filter(|e| e.kind != EventKind::Retract && !withdrawn.contains(&e.seq))
        .map(|e| {
            let mut t = event_to_turn(e);
            if e.kind == EventKind::Note
                && mur_common::channel::RetractRefusal::from_payload(&e.payload).is_some()
            {
                t.role = REFUSAL_ROLE.to_string();
            }
            t
        })
        .collect())
}

/// [`TurnRecord::role`] of a replayed retract-refusal note: a local notice,
/// never an agent reply.
pub const REFUSAL_ROLE: &str = "system";

/// How many of the newest index rows a listing scans for the agent's own.
const CHANNEL_SCAN_LIMIT: usize = 1000;

/// Characters of a listing row's preview.
const PREVIEW_CHARS: usize = 60;

/// Newest conversations with `agent`, newest-first: what `--resume`,
/// `/sessions` and `/channels N` may attach this pane to.
///
/// Conversations only. A fleet or workflow run the agent takes part in is
/// usually the newest channel it is in, and attaching to one sends the next
/// typed line into the run's thread (#2515). Runs are listed by
/// [`list_runs`], under their own heading.
pub fn list_recent(home: &Path, agent: &str, limit: usize) -> Result<Vec<SessionInfo>> {
    list_where(
        home,
        agent,
        limit,
        mur_channel::purpose::is_conversation_with,
    )
}

/// Newest channels of any purpose that involve `agent`, newest-first. For
/// `/channels N --follow` only: tailing reads, so a run stays watchable.
/// Never hand a row from here to anything that writes.
pub fn list_followable(home: &Path, agent: &str, limit: usize) -> Result<Vec<SessionInfo>> {
    list_where(home, agent, limit, mur_channel::purpose::has_agent)
}

/// Newest fleet and workflow runs `agent` takes part in, newest-first: the
/// follow-only rows of the `/channels` listing. Every row here also resolves
/// through [`list_followable`]; none is in [`list_recent`]. Never hand a row
/// from here to anything that writes.
pub fn list_runs(home: &Path, agent: &str, limit: usize) -> Result<Vec<SessionInfo>> {
    list_where(home, agent, limit, mur_channel::purpose::is_run_with)
}

fn list_where(
    home: &Path,
    agent: &str,
    limit: usize,
    keep: fn(&mur_common::channel::Channel, &str) -> bool,
) -> Result<Vec<SessionInfo>> {
    let svc = ChannelService::open(home)?;
    let handles = svc.index().handles()?;
    let mut out = Vec::new();
    for row in svc.list(CHANNEL_SCAN_LIMIT)? {
        let Some(ch) = svc
            .store()
            .load_manifest(&row.id)
            .ok()
            .filter(|ch| keep(ch, agent))
        else {
            continue;
        };
        let evs = svc.load_events(&row.id)?;
        // Skip empty channels (e.g. legacy stubs from before lazy creation) so
        // `--resume`/`/sessions` never surface a blank conversation.
        if evs.is_empty() {
            continue;
        }
        // A run is labelled by its title (`fleet: <name>`, `workflow:
        // <skill>`), which is what tells two runs apart. Not by a human line:
        // a run rarely has one, and where it does (a pane attached by mistake
        // before #2515) the line is not what the run is. A conversation keeps
        // its first human line; its title is not something the user wrote.
        let preview: String = if mur_channel::purpose::is_run_with(&ch, agent) {
            ch.title.chars().take(PREVIEW_CHARS).collect()
        } else {
            evs.iter()
                .find(|e| e.is_human_message())
                .and_then(|e| e.payload.get("text").and_then(|v| v.as_str()))
                .unwrap_or("")
                .chars()
                .take(PREVIEW_CHARS)
                .collect()
        };
        out.push(SessionInfo {
            handle: handles.typeable(&row.id),
            id: row.id,
            preview,
            // The owner record C2 writes ahead of the first human line is
            // bookkeeping, not a turn.
            turns: evs
                .iter()
                .filter(|e| e.kind != EventKind::OwnerKeyEnrolled)
                .count(),
            // No clamp: `ChannelIndex::list` rejects a negative ordinal before
            // it gets here, so this cast cannot silently produce 0 (which
            // already means "not numbered yet").
            ordinal: row.ordinal as u64,
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

    /// An image-only send has no caption. It must never be saved as an empty
    /// user text: that event is replayed as history on every later turn, and an
    /// empty text block makes the provider 400 and wedges the channel.
    #[test]
    fn image_only_user_turn_is_never_saved_blank() {
        let tmp = TempDir::new().unwrap();
        crate::channel_writer::plant_identity_for(tmp.path(), "qa");
        crate::channel_writer::plant_human_identity(tmp.path());
        let mut s = Session::create(tmp.path(), "qa").unwrap();
        s.append("user", "", None, &[]).unwrap();
        s.append("user", "  \n ", None, &[]).unwrap();
        s.append("user", "caption", None, &[]).unwrap();
        let id = s.channel_id().unwrap().to_string();
        let texts: Vec<String> = ChannelService::open(tmp.path())
            .unwrap()
            .load_events(&id)
            .unwrap()
            .iter()
            .filter_map(|e| {
                e.payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                mur_agent_runtime::llm::BLANK_USER_TURN.to_string(),
                mur_agent_runtime::llm::BLANK_USER_TURN.to_string(),
                "caption".to_string(),
            ]
        );
    }

    /// at-mention C1: `append` hands back the seq of the event it wrote, so
    /// the turn can name it as `context.channel_seq`.
    #[test]
    fn append_returns_the_written_events_seq() {
        let tmp = TempDir::new().unwrap();
        crate::channel_writer::plant_identity_for(tmp.path(), "qa");
        crate::channel_writer::plant_human_identity(tmp.path());
        let mut s = Session::create(tmp.path(), "qa").unwrap();
        let first = s.append("user", "one", None, &[]).unwrap();
        let second = s.append("agent", "two", None, &[]).unwrap();
        let third = s.append("shell", "$ ls", None, &[]).unwrap();
        let id = s.channel_id().unwrap().to_string();
        let seqs: Vec<u64> = ChannelService::open(tmp.path())
            .unwrap()
            .load_events(&id)
            .unwrap()
            .iter()
            .filter(|e| e.payload.get("text").is_some())
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, vec![first, second, third]);
        assert!(first < second && second < third);
    }

    #[test]
    fn current_is_none_until_first_append_then_some() {
        let tmp = TempDir::new().unwrap();
        crate::channel_writer::plant_identity_for(tmp.path(), "qa");
        crate::channel_writer::plant_human_identity(tmp.path());
        let mut s = Session::create(tmp.path(), "qa").unwrap();
        assert!(s.current().is_none(), "no channel before first append");
        s.append("user", "hi", None, &[]).unwrap();
        let meta = s.current().expect("channel exists after first append");
        assert!(!meta.id.is_empty());
    }

    /// #716: options offered via `suggest_replies` must land in the agent
    /// event's payload (`suggested_replies`) so channel history is not lossy;
    /// turns without options must not carry the field.
    #[test]
    fn agent_append_persists_offered_suggestions() {
        let tmp = TempDir::new().unwrap();
        crate::channel_writer::plant_identity_for(tmp.path(), "qa");
        crate::channel_writer::plant_human_identity(tmp.path());
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

#[cfg(test)]
#[path = "persist_owner_tests.rs"]
mod owner_tests;

#[cfg(test)]
#[path = "persist_purpose_tests.rs"]
mod purpose_tests;
