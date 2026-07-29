//! `--fleet` status rail: folds a fleet's shared channel into per-member state.

use chrono::{DateTime, Utc};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

/// Most member rows shown when the rail expands. Blocked sorts first, so
/// whatever is truncated is the least urgent.
pub const MAX_EXPANDED_ROWS: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub enum MemberState {
    /// Waiting on a human. `hitl_id` is what `mur channel approve` takes.
    Blocked {
        summary: String,
        hitl_id: String,
    },
    /// `tool` is the latest ToolCall's command; `since` is when the member
    /// last changed state, rendered as elapsed time so a dead runtime shows
    /// up as a growing number instead of a state we invented.
    Working {
        tool: Option<String>,
        since: DateTime<Utc>,
    },
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemberRow {
    pub agent: String,
    pub state: MemberState,
}

impl MemberState {
    /// Sort key: blocked first, then working, then finished.
    fn rank(&self) -> u8 {
        match self {
            MemberState::Blocked { .. } => 0,
            MemberState::Working { .. } => 1,
            MemberState::Done | MemberState::Failed => 2,
        }
    }
}

/// First non-empty string field among `keys`.
fn field<'a>(ev: &'a ChannelEvent, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| ev.payload.get(*k).and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
}

/// Fold a channel's events into one row per agent that has acted.
///
/// Only `ChannelActor::Agent` becomes a row: `Human` events are the user's own
/// turns and `System` events are the executor's bookkeeping. A member with no
/// events is absent rather than "idle" — silence is not a state we can read.
pub fn fold_members(events: &[ChannelEvent]) -> Vec<MemberRow> {
    // Insertion-ordered so the sort below is the only thing that reorders.
    let mut rows: Vec<MemberRow> = Vec::new();
    let index = |rows: &mut Vec<MemberRow>, id: &str, ts: DateTime<Utc>| -> usize {
        if let Some(i) = rows.iter().position(|r| r.agent == id) {
            return i;
        }
        rows.push(MemberRow {
            agent: id.to_string(),
            state: MemberState::Working {
                tool: None,
                since: ts,
            },
        });
        rows.len() - 1
    };

    for ev in events {
        // A HitlResponse is written by whoever approved — usually the human —
        // so it is matched by hitl_id across ALL rows, not by actor.
        if ev.kind == EventKind::HitlResponse
            && let Some(id) = field(ev, &["hitl_id"])
        {
            for row in rows.iter_mut() {
                if let MemberState::Blocked { hitl_id, .. } = &row.state
                    && hitl_id == id
                {
                    row.state = MemberState::Working {
                        tool: None,
                        since: ev.ts,
                    };
                }
            }
            continue;
        }

        let ChannelActor::Agent { id } = &ev.actor else {
            continue;
        };

        match ev.kind {
            EventKind::StateChange => {
                let Some(to) = field(ev, &["to"]) else {
                    continue;
                };
                let state = match to {
                    // ChannelState serializes kebab-case (see channel.rs tests).
                    "working" | "submitted" => MemberState::Working {
                        tool: None,
                        since: ev.ts,
                    },
                    "input-required" => MemberState::Blocked {
                        summary: "waiting for input".to_string(),
                        hitl_id: String::new(),
                    },
                    "completed" => MemberState::Done,
                    "failed" | "canceled" | "rejected" => MemberState::Failed,
                    _ => continue,
                };
                let i = index(&mut rows, id, ev.ts);
                rows[i].state = state;
            }
            EventKind::ToolCall => {
                let tool = field(ev, &["command", "tool", "description"]).map(str::to_string);
                let i = index(&mut rows, id, ev.ts);
                // A tool call only annotates a running member; it must not
                // resurrect one that already finished or unblock one waiting.
                if let MemberState::Working { since, .. } = rows[i].state {
                    rows[i].state = MemberState::Working { tool, since };
                }
            }
            EventKind::HitlRequest => {
                let i = index(&mut rows, id, ev.ts);
                rows[i].state = MemberState::Blocked {
                    summary: field(ev, &["summary", "tool_name"])
                        .unwrap_or("approval needed")
                        .to_string(),
                    hitl_id: field(ev, &["hitl_id"]).unwrap_or_default().to_string(),
                };
            }
            _ => {}
        }
    }

    rows.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then_with(|| a.agent.cmp(&b.agent))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(
        seq: u64,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> ChannelEvent {
        ChannelEvent {
            seq,
            ts: DateTime::parse_from_rfc3339("2026-07-29T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            actor,
            kind,
            payload,
            idempotency_key: None,
            sig: None,
            key_version: None,
        }
    }

    fn agent(id: &str) -> ChannelActor {
        ChannelActor::Agent { id: id.into() }
    }

    #[test]
    fn state_changes_map_to_member_states() {
        let evs = vec![
            ev(
                1,
                agent("qa"),
                EventKind::StateChange,
                json!({"from": "submitted", "to": "working"}),
            ),
            ev(
                2,
                agent("backend"),
                EventKind::StateChange,
                json!({"from": "working", "to": "completed"}),
            ),
            ev(
                3,
                agent("dataml"),
                EventKind::StateChange,
                json!({"from": "working", "to": "failed"}),
            ),
            ev(
                4,
                agent("pm"),
                EventKind::StateChange,
                json!({"from": "working", "to": "canceled"}),
            ),
        ];
        let rows = fold_members(&evs);
        let by = |n: &str| rows.iter().find(|r| r.agent == n).unwrap().state.clone();
        assert!(matches!(by("qa"), MemberState::Working { .. }));
        assert!(matches!(by("backend"), MemberState::Done));
        // canceled and rejected collapse into failed — the user only needs
        // "it did not finish".
        assert!(matches!(by("dataml"), MemberState::Failed));
        assert!(matches!(by("pm"), MemberState::Failed));
    }

    #[test]
    fn a_hitl_request_blocks_and_its_response_unblocks() {
        let req = json!({"hitl_id": "h1", "tool_name": "bash", "summary": "cargo publish", "action_hash": "x", "tier": "write"});
        let evs = vec![
            ev(
                1,
                agent("qa"),
                EventKind::StateChange,
                json!({"to": "working"}),
            ),
            ev(2, agent("qa"), EventKind::HitlRequest, req),
        ];
        let rows = fold_members(&evs);
        match &rows[0].state {
            MemberState::Blocked { summary, hitl_id } => {
                assert_eq!(hitl_id, "h1");
                assert!(summary.contains("cargo publish"));
            }
            other => panic!("expected blocked, got {other:?}"),
        }

        // The approval is written by the HUMAN, not by the blocked agent, so
        // clearing must key on hitl_id — never on the actor.
        let mut evs = evs;
        evs.push(ev(
            3,
            ChannelActor::Human {
                name: "david".into(),
            },
            EventKind::HitlResponse,
            json!({"hitl_id": "h1", "allow": true, "surface": "cli"}),
        ));
        let rows = fold_members(&evs);
        assert!(matches!(rows[0].state, MemberState::Working { .. }));
    }

    #[test]
    fn tool_calls_annotate_the_working_row() {
        let evs = vec![
            ev(
                1,
                agent("qa"),
                EventKind::StateChange,
                json!({"to": "working"}),
            ),
            ev(
                2,
                agent("qa"),
                EventKind::ToolCall,
                json!({"tool": "bash", "command": "cargo test"}),
            ),
        ];
        let rows = fold_members(&evs);
        match &rows[0].state {
            MemberState::Working { tool, .. } => assert_eq!(tool.as_deref(), Some("cargo test")),
            other => panic!("expected working, got {other:?}"),
        }
    }

    #[test]
    fn human_and_system_actors_never_become_rows() {
        let evs = vec![
            ev(
                1,
                ChannelActor::Human {
                    name: "david".into(),
                },
                EventKind::Message,
                json!({"text": "go"}),
            ),
            ev(
                2,
                ChannelActor::System,
                EventKind::StateChange,
                json!({"to": "working"}),
            ),
        ];
        assert!(fold_members(&evs).is_empty());
    }

    #[test]
    fn blocked_sorts_first_then_working_then_finished() {
        let evs = vec![
            ev(
                1,
                agent("aaa_done"),
                EventKind::StateChange,
                json!({"to": "completed"}),
            ),
            ev(
                2,
                agent("bbb_working"),
                EventKind::StateChange,
                json!({"to": "working"}),
            ),
            ev(
                3,
                agent("ccc_blocked"),
                EventKind::HitlRequest,
                json!({"hitl_id": "h1", "tool_name": "bash", "summary": "rm", "action_hash": "x", "tier": "write"}),
            ),
        ];
        let rows = fold_members(&evs);
        let names: Vec<&str> = rows.iter().map(|r| r.agent.as_str()).collect();
        assert_eq!(names, vec!["ccc_blocked", "bbb_working", "aaa_done"]);
    }

    #[test]
    fn an_empty_channel_has_no_rows() {
        assert!(fold_members(&[]).is_empty());
    }
}
