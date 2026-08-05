//! `/channels N --follow` — live-tail ANOTHER channel while you keep chatting
//! in your own.
//!
//! Work that MUR (or a fleet) delegates to this agent lands in a *shared*
//! channel, not the one this pane chats on — the pane's stream is turn-scoped
//! and only ever shows the task it dialed itself. Following that channel is how
//! the operator watches delegated work land, and spots a HITL gate, without
//! switching away from their own conversation (a switch only ever shows a disk
//! snapshot, and would then mix the operator's own turns into it).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_channel::ChannelStore;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::hitl::{HitlRequest, HitlResponse};

/// How often a followed channel is re-read. Each poll is one `metadata()` call
/// unless the log actually grew, so this is not a hot loop.
pub const POLL_INTERVAL: Duration = Duration::from_millis(700);

/// Longest event text carried into one transcript line.
const TEXT_MAX: usize = 400;

/// Milestone lines are conversation furniture, not a log dump — keep them
/// tighter than raw-follow lines.
const MILESTONE_MAX: usize = 160;

/// How a followed channel renders into transcript lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FollowMode {
    /// Every event, one line each (`/channels N --follow`) — the debug view.
    #[default]
    Raw,
    /// Transitions only: delegations, member completions, run outcomes, HITL.
    /// What the auto-armed fleet follow uses.
    Milestone,
}

/// A followed channel: a byte-length gate plus a `seq` cursor over its
/// append-only log.
pub struct Follow {
    pub channel_id: String,
    /// NEXT `seq` to render — the dedup cursor. Starts one past the log's
    /// current head so following never replays history. ("One past" and not
    /// "highest rendered" because seq starts at 0: a follow armed on an empty
    /// channel must not eat that first event.)
    cursor: u64,
    /// Log size at the last poll. Unchanged size means nothing to parse.
    /// ponytail: re-parses the whole log when it grows; switch to a byte-offset
    /// seek if a followed channel ever gets long enough to notice.
    last_len: u64,
    pub next_poll: Instant,
    /// How events become lines (raw tail vs milestone transitions).
    pub mode: FollowMode,
    /// True when armed automatically by an in-flight `fleet_run` step rather
    /// than by the user — the auto follow is stopped when that step completes;
    /// a user-armed follow never is.
    pub auto: bool,
    /// Milestone mode only: when each delegated member's turn started
    /// (`Delegation` event ts), so its completion line carries an elapsed
    /// duration.
    delegated_at: HashMap<String, DateTime<Utc>>,
}

impl Follow {
    /// Start following `channel_id` from its current head.
    pub fn start(home: &Path, channel_id: &str, now: Instant) -> Result<Self> {
        let store = ChannelStore::new(home);
        let evs = store.load_events(channel_id)?;
        Ok(Self {
            channel_id: channel_id.to_string(),
            cursor: evs.last().map(|e| e.seq + 1).unwrap_or(0),
            last_len: log_len(&store, channel_id),
            next_poll: now + POLL_INTERVAL,
            mode: FollowMode::Raw,
            auto: false,
            delegated_at: HashMap::new(),
        })
    }

    /// Start following `channel_id` in milestone mode, armed by the TUI itself
    /// when a delegated `fleet_run` step begins. Infallible on purpose: the
    /// fleet's channel may not exist yet at arm time (the run's first event
    /// creates it), so a missing log reads as an empty one and the cursor
    /// starts at whatever head exists.
    pub fn start_auto(home: &Path, channel_id: &str, now: Instant) -> Self {
        let store = ChannelStore::new(home);
        let cursor = store
            .load_events(channel_id)
            .ok()
            .and_then(|evs| evs.last().map(|e| e.seq + 1))
            .unwrap_or(0);
        Self {
            channel_id: channel_id.to_string(),
            cursor,
            last_len: log_len(&store, channel_id),
            next_poll: now + POLL_INTERVAL,
            mode: FollowMode::Milestone,
            auto: true,
            delegated_at: HashMap::new(),
        }
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
        for ev in evs.iter().filter(|e| e.seq >= cursor) {
            self.cursor = ev.seq + 1;
            match self.mode {
                FollowMode::Raw => out.push(format!(
                    "⟨{}⟩ {}",
                    self.tag(),
                    summarize(ev, &self.channel_id)
                )),
                FollowMode::Milestone => {
                    if let Some(line) = milestone(ev, &self.channel_id, &mut self.delegated_at) {
                        out.push(line);
                    }
                }
            }
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

/// One transcript line for a milestone-worthy event — `None` for everything
/// else. Milestones are the transitions a user acts on or anchors to:
/// delegation (turn start), a member's own completion summary (turn end), run
/// outcomes, loop-guard notes, plus everything `summarize` already renders
/// loudly (HITL, tool calls, artifacts). Working-state flips and human goal
/// relays stay off the transcript — the rail carries "now"; the transcript
/// keeps history.
pub fn milestone(
    ev: &ChannelEvent,
    channel_id: &str,
    delegated_at: &mut HashMap<String, DateTime<Utc>>,
) -> Option<String> {
    match ev.kind {
        EventKind::Delegation => {
            let target = field(ev, &["target_agent"])?;
            delegated_at.insert(target.to_string(), ev.ts);
            let goal = field(ev, &["goal"])
                .map(|g| format!(" — {}", clip_to(g, MILESTONE_MAX)))
                .unwrap_or_default();
            Some(format!("▸ delegated → {target}{goal}"))
        }
        EventKind::Message => {
            // Only a member's own (signed) reply is a milestone — its first
            // line is the member-authored summary of the turn.
            let ChannelActor::Agent { id } = &ev.actor else {
                return None;
            };
            let text = field(ev, &["text"]).unwrap_or("");
            let first = text
                .lines()
                .map(|l| l.trim_start_matches('#').trim())
                .find(|l| !l.is_empty())
                .unwrap_or("");
            let took = delegated_at
                .remove(id.as_str())
                .map(|t| format!(" ({})", fmt_elapsed(ev.ts.signed_duration_since(t))))
                .unwrap_or_default();
            Some(format!("✓ {id}{took} — {}", clip_to(first, MILESTONE_MAX)))
        }
        // Channel-level transitions: one line per finished run (each loop
        // iteration is one run). `working` flips are rail material, not
        // transcript material.
        EventKind::StateChange => match field(ev, &["to"])? {
            "completed" => Some("✓ run completed".to_string()),
            to @ ("failed" | "canceled" | "rejected") => Some(format!("✗ run {to}")),
            _ => None,
        },
        // Loop-guard notes (budget exhausted, stuck detection, …) are exactly
        // what an operator wants on the record.
        EventKind::Note => Some(format!(
            "· {}",
            clip_to(field(ev, &["text"])?, MILESTONE_MAX)
        )),
        EventKind::HitlRequest
        | EventKind::HitlResponse
        | EventKind::ToolCall
        | EventKind::ToolResult
        | EventKind::Artifact => Some(summarize(ev, channel_id)),
        _ => None,
    }
}

/// `45s`, `2m10s`, `1h3m` — coarse on purpose; these are history lines.
pub(crate) fn fmt_elapsed(d: chrono::Duration) -> String {
    let s = d.num_seconds().max(0);
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{}s", s / 60, s % 60)
    } else {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    }
}

fn step(ev: &ChannelEvent) -> String {
    field(ev, &["step_id"])
        .map(|s| format!(" [{s}]"))
        .unwrap_or_default()
}

fn clip(s: &str) -> String {
    clip_to(s, TEXT_MAX)
}

fn clip_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
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

    // ── Milestone mode ──────────────────────────────────────────────────────

    fn mk(
        seq: u64,
        secs: i64,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> ChannelEvent {
        ChannelEvent {
            seq,
            ts: chrono::DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap(),
            actor,
            kind,
            payload,
            idempotency_key: None,
            sig: None,
            key_version: None,
        }
    }

    #[test]
    fn milestone_delegation_then_reply_carries_elapsed() {
        let mut at = HashMap::new();
        let d = mk(
            1,
            0,
            ChannelActor::System,
            EventKind::Delegation,
            serde_json::json!({"target_agent": "dr_worker_1", "goal": "survey 2026 agent memory designs"}),
        );
        let line = milestone(&d, "fleet-x", &mut at).unwrap();
        assert!(line.contains("delegated → dr_worker_1"), "got: {line}");
        assert!(line.contains("survey 2026"), "got: {line}");

        let m = mk(
            2,
            130,
            ChannelActor::Agent {
                id: "dr_worker_1".into(),
            },
            EventKind::Message,
            serde_json::json!({"text": "## Summary\n\nAll sources verified — synthesizing."}),
        );
        let line = milestone(&m, "fleet-x", &mut at).unwrap();
        assert_eq!(line, "✓ dr_worker_1 (2m10s) — Summary", "got: {line}");
        assert!(at.is_empty(), "the reply consumes the delegation timestamp");
    }

    #[test]
    fn milestone_skips_noise_and_keeps_outcomes() {
        let mut at = HashMap::new();
        let human = mk(
            1,
            0,
            ChannelActor::Human { name: "d".into() },
            EventKind::Message,
            serde_json::json!({"text": "the goal"}),
        );
        assert!(milestone(&human, "c", &mut at).is_none());
        let working = mk(
            2,
            0,
            ChannelActor::System,
            EventKind::StateChange,
            serde_json::json!({"from": "submitted", "to": "working"}),
        );
        assert!(milestone(&working, "c", &mut at).is_none());
        let done = mk(
            3,
            0,
            ChannelActor::System,
            EventKind::StateChange,
            serde_json::json!({"from": "working", "to": "completed"}),
        );
        assert_eq!(milestone(&done, "c", &mut at).unwrap(), "✓ run completed");
        let failed = mk(
            4,
            0,
            ChannelActor::System,
            EventKind::StateChange,
            serde_json::json!({"from": "working", "to": "failed"}),
        );
        assert_eq!(milestone(&failed, "c", &mut at).unwrap(), "✗ run failed");
        let note = mk(
            5,
            0,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({"text": "budget exhausted"}),
        );
        assert_eq!(
            milestone(&note, "c", &mut at).unwrap(),
            "· budget exhausted"
        );
    }

    #[test]
    fn fmt_elapsed_buckets() {
        use chrono::Duration as D;
        assert_eq!(fmt_elapsed(D::seconds(45)), "45s");
        assert_eq!(fmt_elapsed(D::seconds(130)), "2m10s");
        assert_eq!(fmt_elapsed(D::seconds(3780)), "1h3m");
        assert_eq!(fmt_elapsed(D::seconds(-5)), "0s");
    }

    /// `start_auto` on a channel that does not exist yet must arm quietly and
    /// then surface milestone lines once the run's first events land.
    #[test]
    fn start_auto_tolerates_missing_channel_and_drains_milestones() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let now = Instant::now();
        let mut f = Follow::start_auto(home, "fleet-x", now);
        assert!(f.auto);
        append(
            home,
            "fleet-x",
            EventKind::Delegation,
            serde_json::json!({"target_agent": "qa", "goal": "fix tests"}),
        );
        let lines = f.drain(home, now).unwrap();
        assert_eq!(lines, vec!["▸ delegated → qa — fix tests".to_string()]);
    }
}
