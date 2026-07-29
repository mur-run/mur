//! `--fleet` status rail: folds a fleet's shared channel into per-member state.

use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, Utc};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::fleet::{Job, JobStatus};

use super::follow::POLL_INTERVAL;

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

/// The always-present collapsed line: how far the fleet's work has got.
///
/// `2/5` is jobs in a terminal state over the total — the question a user asks
/// first ("how far along?"), answered by the slow-moving store rather than by
/// the event stream.
pub fn jobs_line(fleet: &str, jobs: &[Job]) -> String {
    if jobs.is_empty() {
        return format!("fleet · {fleet}   not run yet (mur fleet run {fleet})");
    }
    let total = jobs.len();
    let terminal = jobs.iter().filter(|j| j.status.is_terminal()).count();
    let running = jobs
        .iter()
        .filter(|j| j.status == JobStatus::Running)
        .count();
    let failed = jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Failed | JobStatus::Canceled))
        .count();
    let mut line = format!("fleet · {fleet}   job {terminal}/{total}");
    if running > 0 {
        line.push_str(&format!(" · {running} ⏵ running"));
    }
    if failed > 0 {
        line.push_str(&format!(" · {failed} ✖ failed"));
    }
    line
}

/// What the band renders. Recomputed only when the channel log or the job
/// store actually changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RailView {
    pub jobs_line: String,
    pub members: Vec<MemberRow>,
    /// Degraded-state text (unreadable channel, unreadable jobs). Rendered in
    /// place of detail; never an error the caller has to handle.
    pub notice: Option<String>,
}

/// Polls one fleet's channel and job store, folding both into a `RailView`.
///
/// Deliberately a separate type from `Follow`: that one turns events into
/// transcript lines (history, reaches scrollback), this one folds them into
/// current state (repainted every frame, never flushed). Keeping them apart also
/// leaves `app.follow` free, so `/channels <id> --follow` still works while a
/// rail is up.
pub struct FleetRail {
    fleet: String,
    channel_id: String,
    /// Channel log size at the last poll; unchanged size means nothing to parse.
    last_len: u64,
    /// (entry count, newest mtime) seen in the jobs dir at the last poll. The
    /// count catches a deletion that `max(mtime)` alone would miss — removing
    /// any job that isn't the newest leaves the max unchanged and the gate
    /// blind to a real change.
    last_jobs_gate: (usize, Option<std::time::SystemTime>),
    view: RailView,
    next_poll: Instant,
}

impl FleetRail {
    pub fn start(fleet: &str) -> Self {
        Self {
            fleet: fleet.to_string(),
            channel_id: format!("fleet-{fleet}"),
            last_len: u64::MAX, // force the first poll to do real work
            last_jobs_gate: (0, None),
            view: RailView::default(),
            next_poll: Instant::now(),
        }
    }

    pub fn view(&self) -> &RailView {
        &self.view
    }

    pub fn next_poll(&self) -> Instant {
        self.next_poll
    }

    /// Recompute if anything moved. Returns true when the view changed, so the
    /// caller redraws only then.
    pub fn poll(&mut self, home: &Path, now: Instant) -> bool {
        self.next_poll = now + POLL_INTERVAL;
        let store = mur_channel::ChannelStore::new(home);
        let len = std::fs::metadata(store.events_path(&self.channel_id))
            .map(|m| m.len())
            .unwrap_or(0);
        let jobs_gate = jobs_dir_gate(home, &self.fleet);
        if len == self.last_len && jobs_gate == self.last_jobs_gate {
            return false;
        }
        self.last_len = len;
        self.last_jobs_gate = jobs_gate;

        let mut notices: Vec<String> = Vec::new();
        // `load_events` treats a missing log as an empty one (Ok(vec![])) so a
        // brand-new channel with zero events isn't an error — but that means it
        // can't distinguish "this fleet's channel doesn't exist" from "it exists
        // and is quiet". The manifest is the fleet's actual existence check.
        if store.load_manifest(&self.channel_id).is_err() {
            notices.push("⚠ channel unreadable".to_string());
        }
        let events = match store.load_events(&self.channel_id) {
            Ok(evs) => {
                // Same trust rule as every other fold: an event that fails its
                // actor's signature is dropped, never rendered. The rail
                // vouches for OTHER agents, so showing an unverified "done"
                // would lend the UI's credibility to a forgery.
                let require_sig = crate::channel_verify::require_sig_from_env();
                verify_events(home, &self.channel_id, evs, require_sig)
            }
            Err(_) => {
                notices.push("⚠ channel unreadable".to_string());
                Vec::new()
            }
        };

        let members = fold_members(&events);

        // "Load once, use twice" (spec §3): the events already loaded and
        // verified above also feed job reconciliation, the same convergence
        // `mur fleet show`/`mur fleet jobs` apply via `reconcile_jobs` — but
        // applied in memory only (no `save_job`); the rail stays read-only.
        let channel_terminal = crate::cmd::fleet::jobs::channel_terminal_status(&events);
        let jobs_line_text = match crate::cmd::fleet::jobs::list_jobs_raw(home, &self.fleet) {
            Ok(mut jobs) => {
                let now_utc = Utc::now();
                for j in jobs.iter_mut() {
                    if let Some((s, _)) =
                        crate::cmd::fleet::jobs::reconcile_running(j, channel_terminal, now_utc)
                    {
                        j.status = s;
                    }
                }
                jobs_line(&self.fleet, &jobs)
            }
            Err(_) => {
                // A corrupt job file must not read as "not run yet" (`jobs_line`
                // on an empty Vec would say exactly that) — fall back to a
                // summary derived from the channel fold instead (spec §4).
                notices.push("⚠ jobs unreadable".to_string());
                channel_derived_line(&self.fleet, &members)
            }
        };

        let view = RailView {
            jobs_line: jobs_line_text,
            members,
            notice: if notices.is_empty() {
                None
            } else {
                Some(notices.join("  "))
            },
        };
        let changed = view != self.view;
        self.view = view;
        changed
    }
}

/// Fallback collapsed line when the job store is unreadable (spec §4): counts
/// derived from the channel fold instead of the job store, since the store is
/// exactly what's unavailable.
fn channel_derived_line(fleet: &str, members: &[MemberRow]) -> String {
    let done = members
        .iter()
        .filter(|m| m.state == MemberState::Done)
        .count();
    let working = members
        .iter()
        .filter(|m| matches!(m.state, MemberState::Working { .. }))
        .count();
    let blocked = members
        .iter()
        .filter(|m| matches!(m.state, MemberState::Blocked { .. }))
        .count();
    let failed = members
        .iter()
        .filter(|m| m.state == MemberState::Failed)
        .count();
    let mut line = format!("fleet · {fleet}   member {done}/{} done", members.len());
    if working > 0 {
        line.push_str(&format!(" · {working} ⏵ working"));
    }
    if blocked > 0 {
        line.push_str(&format!(" · {blocked} ▲ blocked"));
    }
    if failed > 0 {
        line.push_str(&format!(" · {failed} ✖ failed"));
    }
    line
}

/// Verify each event against its actor's pubkey, resolving each distinct
/// actor's key ONCE per poll rather than once per event. `actor_pubkey` does
/// up to two file reads plus a rotation-chain verify; on a channel with
/// thousands of events, re-resolving per event means thousands of synchronous
/// file reads on the event-loop thread every ~700ms. Semantics are identical
/// to `channel_verify::verify_event` per event, including its
/// `None => !require_sig` fallback for an actor whose key can't be resolved.
fn verify_events(
    home: &Path,
    channel_id: &str,
    events: Vec<ChannelEvent>,
    require_sig: bool,
) -> Vec<ChannelEvent> {
    let mut cache: std::collections::HashMap<(String, Option<u32>), Option<[u8; 32]>> =
        std::collections::HashMap::new();
    events
        .into_iter()
        .filter(|ev| {
            let agent = match &ev.actor {
                ChannelActor::Agent { id } => id.as_str(),
                _ => crate::channel_writer::ROUTER_AGENT,
            };
            let pk = *cache
                .entry((agent.to_string(), ev.key_version))
                .or_insert_with(|| {
                    mur_channel::sign::resolve_writer_pubkey(
                        &home.join("agents").join(agent),
                        ev.key_version,
                    )
                });
            match pk {
                Some(pk) => mur_channel::sign::verify_one(channel_id, ev, &pk, require_sig),
                None => !require_sig,
            }
        })
        .collect()
}

/// (entry count, newest mtime) across the fleet's job files — the cheap "did
/// jobs move?" gate. The count is load-bearing: deleting a non-newest job
/// leaves `max(mtime)` unchanged, so mtime alone misses it.
fn jobs_dir_gate(home: &Path, fleet: &str) -> (usize, Option<std::time::SystemTime>) {
    let dir = crate::cmd::fleet::jobs::jobs_dir(home, fleet);
    let entries: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().collect())
        .unwrap_or_default();
    let count = entries.len();
    let max_mtime = entries
        .iter()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max();
    (count, max_mtime)
}

#[cfg(test)]
#[path = "fleet_rail/tests.rs"]
mod tests;
