//! `/channels N --follow` — live-tail ANOTHER channel while you keep chatting
//! in your own.
//!
//! Work that MUR (or a fleet) delegates to this agent lands in a *shared*
//! channel, not the one this pane chats on — the pane's stream is turn-scoped
//! and only ever shows the task it dialed itself. Following that channel is how
//! the operator watches delegated work land, and spots a HITL gate, without
//! switching away from their own conversation (a switch only ever shows a disk
//! snapshot, and would then mix the operator's own turns into it).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use mur_channel::ChannelStore;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::hitl::{HitlRequest, HitlResponse};

/// How often a followed channel is re-read. Each poll is one `metadata()` call
/// unless the log actually grew, so this is not a hot loop.
pub const POLL_INTERVAL: Duration = Duration::from_millis(700);

/// Longest event text carried into one transcript line.
const TEXT_MAX: usize = 400;

/// A followed channel: a byte-length gate plus a `seq` cursor over its
/// append-only log.
pub struct Follow {
    pub channel_id: String,
    /// Highest `seq` already rendered — the dedup cursor. Starts at the log's
    /// current head so following never replays history.
    cursor: u64,
    /// Log size at the last poll. Unchanged size means nothing to parse.
    /// ponytail: re-parses the whole log when it grows; switch to a byte-offset
    /// seek if a followed channel ever gets long enough to notice.
    last_len: u64,
    pub next_poll: Instant,
}

impl Follow {
    /// Start following `channel_id` from its current head.
    pub fn start(home: &Path, channel_id: &str, now: Instant) -> Result<Self> {
        let store = ChannelStore::new(home);
        let evs = store.load_events(channel_id)?;
        Ok(Self {
            channel_id: channel_id.to_string(),
            cursor: evs.last().map(|e| e.seq).unwrap_or(0),
            last_len: log_len(&store, channel_id),
            next_poll: now + POLL_INTERVAL,
        })
    }

    /// Short display tag for transcript lines.
    pub fn tag(&self) -> &str {
        &self.channel_id[..self.channel_id.len().min(8)]
    }

    /// Lines for events that landed since the last poll, oldest-first. Empty
    /// when nothing landed.
    pub fn drain(&mut self, home: &Path, now: Instant) -> Result<Vec<String>> {
        self.next_poll = now + POLL_INTERVAL;
        let store = ChannelStore::new(home);
        let len = log_len(&store, &self.channel_id);
        if len == self.last_len {
            return Ok(Vec::new());
        }
        self.last_len = len;
        let evs = store.load_events(&self.channel_id)?;
        let mut out = Vec::new();
        let cursor = self.cursor;
        for ev in evs.iter().filter(|e| e.seq > cursor) {
            self.cursor = ev.seq;
            out.push(format!(
                "⟨{}⟩ {}",
                self.tag(),
                summarize(ev, &self.channel_id)
            ));
        }
        Ok(out)
    }
}

fn log_len(store: &ChannelStore, id: &str) -> u64 {
    std::fs::metadata(store.events_path(id))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// One transcript line for a followed event: who acted, then a kind-specific
/// body. A HITL gate carries the exact approve command — that is the whole
/// point of watching, so it must not be one indirection away.
pub fn summarize(ev: &ChannelEvent, channel_id: &str) -> String {
    let who = match &ev.actor {
        ChannelActor::Agent { id } => id.as_str(),
        ChannelActor::Human { .. } => "human",
        ChannelActor::System => "system",
    };
    let body = match ev.kind {
        EventKind::Message | EventKind::Note | EventKind::Delegation | EventKind::Handoff => {
            clip(field(ev, &["text"]).unwrap_or(""))
        }
        EventKind::ToolCall => format!(
            "→ {}{}",
            field(ev, &["tool", "command", "description"]).unwrap_or("tool"),
            step(ev)
        ),
        EventKind::ToolResult => {
            let ok = ev
                .payload
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let code = ev
                .payload
                .get("exit_code")
                .and_then(|v| v.as_i64())
                .unwrap_or(-1);
            let out = field(ev, &["output"]).unwrap_or("");
            let head = out.lines().next().unwrap_or("");
            format!(
                "{} {}exit {code}{}",
                if ok { "✓" } else { "✗" },
                step(ev).trim_start(),
                if head.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", clip(head))
                }
            )
        }
        EventKind::StateChange => format!(
            "state: {} → {}",
            field(ev, &["from"]).unwrap_or("?"),
            field(ev, &["to"]).unwrap_or("?")
        ),
        EventKind::Artifact => format!("artifact: {}", field(ev, &["path", "name"]).unwrap_or("?")),
        EventKind::HitlRequest => match serde_json::from_value::<HitlRequest>(ev.payload.clone()) {
            Ok(r) => format!(
                "⏸ approval needed: {} ({}) · !mur channel approve {channel_id} {}",
                clip(&r.summary),
                r.tool_name,
                r.hitl_id
            ),
            Err(_) => "⏸ approval needed (unreadable request)".to_string(),
        },
        EventKind::HitlResponse => match serde_json::from_value::<HitlResponse>(ev.payload.clone())
        {
            Ok(r) => format!(
                "{} {} ({})",
                if r.allow { "approved" } else { "denied" },
                r.hitl_id,
                r.surface
            ),
            Err(_) => "approval decision (unreadable)".to_string(),
        },
    };
    format!("{who}: {body}")
}

/// First present string field among `keys`.
fn field<'a>(ev: &'a ChannelEvent, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| ev.payload.get(*k).and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
}

fn step(ev: &ChannelEvent) -> String {
    field(ev, &["step_id"])
        .map(|s| format!(" [{s}]"))
        .unwrap_or_default()
}

fn clip(s: &str) -> String {
    if s.chars().count() <= TEXT_MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(TEXT_MAX).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_channel::ChannelService;
    use mur_common::channel::{ChannelActor, EventKind};
    use tempfile::tempdir;

    fn append(home: &Path, cid: &str, kind: EventKind, payload: serde_json::Value) {
        let svc = ChannelService::open(home).unwrap();
        svc.append(cid, ChannelActor::System, kind, payload, None)
            .unwrap();
    }

    /// The whole contract: history is never replayed, new events surface once,
    /// and a quiet channel yields nothing.
    #[test]
    fn follows_from_head_without_replaying_or_repeating() {
        let home = tempdir().unwrap();
        let svc = ChannelService::open(home.path()).unwrap();
        let ch = svc.create_for_agent("rustsmith").unwrap();
        append(
            home.path(),
            &ch.id,
            EventKind::Message,
            serde_json::json!({"text": "before following"}),
        );

        let now = Instant::now();
        let mut f = Follow::start(home.path(), &ch.id, now).unwrap();
        assert!(
            f.drain(home.path(), now).unwrap().is_empty(),
            "history must not replay"
        );

        append(
            home.path(),
            &ch.id,
            EventKind::ToolCall,
            serde_json::json!({"step_id": "s1", "tool": "bash"}),
        );
        let lines = f.drain(home.path(), now).unwrap();
        assert_eq!(lines.len(), 1, "one new event → one line: {lines:?}");
        assert!(
            lines[0].contains("bash") && lines[0].contains("s1"),
            "{lines:?}"
        );
        assert!(
            f.drain(home.path(), now).unwrap().is_empty(),
            "same event must not surface twice"
        );
    }

    /// A gate is only useful if the line tells you how to answer it.
    #[test]
    fn hitl_request_line_carries_the_approve_command() {
        let req = mur_common::hitl::HitlRequest {
            hitl_id: "h7".into(),
            action_hash: "abc".into(),
            tier: mur_common::hitl::RiskTier::Write,
            tool_name: "bash".into(),
            tool_input: serde_json::json!({"command": "rm -rf x"}),
            step_or_call_id: "s2".into(),
            agent_id: "rustsmith".into(),
            timeout_ms: 1000,
            summary: "delete x".into(),
        };
        let ev = ChannelEvent {
            seq: 1,
            ts: chrono::Utc::now(),
            actor: ChannelActor::System,
            kind: EventKind::HitlRequest,
            payload: serde_json::to_value(&req).unwrap(),
            idempotency_key: None,
            sig: None,
            key_version: None,
        };
        let line = summarize(&ev, "ch1");
        assert!(line.contains("mur channel approve ch1 h7"), "{line}");
    }
}
