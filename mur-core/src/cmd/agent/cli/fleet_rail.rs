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
    /// Newest mtime seen in the jobs dir; jobs move far slower than events.
    last_jobs_mtime: Option<std::time::SystemTime>,
    view: RailView,
    next_poll: Instant,
}

impl FleetRail {
    pub fn start(home: &Path, fleet: &str) -> Self {
        let _ = home;
        Self {
            fleet: fleet.to_string(),
            channel_id: format!("fleet-{fleet}"),
            last_len: u64::MAX, // force the first poll to do real work
            last_jobs_mtime: None,
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
        let jobs_mtime = newest_jobs_mtime(home, &self.fleet);
        if len == self.last_len && jobs_mtime == self.last_jobs_mtime {
            return false;
        }
        self.last_len = len;
        self.last_jobs_mtime = jobs_mtime;

        let mut notice = None;
        // `load_events` treats a missing log as an empty one (Ok(vec![])) so a
        // brand-new channel with zero events isn't an error — but that means it
        // can't distinguish "this fleet's channel doesn't exist" from "it exists
        // and is quiet". The manifest is the fleet's actual existence check.
        if store.load_manifest(&self.channel_id).is_err() {
            notice = Some("⚠ channel unreadable".to_string());
        }
        let events = match store.load_events(&self.channel_id) {
            Ok(evs) => {
                // Same trust rule as every other fold: an event that fails its
                // actor's signature is dropped, never rendered. The rail
                // vouches for OTHER agents, so showing an unverified "done"
                // would lend the UI's credibility to a forgery.
                let require_sig = std::env::var("MUR_CHANNEL_REQUIRE_SIG")
                    .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"))
                    .unwrap_or(false);
                evs.into_iter()
                    .filter(|e| {
                        crate::channel_verify::verify_event(home, &self.channel_id, e, require_sig)
                    })
                    .collect::<Vec<_>>()
            }
            Err(_) => {
                notice = Some("⚠ channel unreadable".to_string());
                Vec::new()
            }
        };

        let jobs = crate::cmd::fleet::jobs::list_jobs_raw(home, &self.fleet).unwrap_or_default();
        let view = RailView {
            jobs_line: jobs_line(&self.fleet, &jobs),
            members: fold_members(&events),
            notice,
        };
        let changed = view != self.view;
        self.view = view;
        changed
    }
}

/// Newest mtime across the fleet's job files — the cheap "did jobs move?" gate.
fn newest_jobs_mtime(home: &Path, fleet: &str) -> Option<std::time::SystemTime> {
    let dir = crate::cmd::fleet::jobs::jobs_dir(home, fleet);
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
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

    use mur_common::fleet::{Job, JobStatus};

    fn job(id: &str, status: JobStatus) -> Job {
        Job {
            id: id.into(),
            text: "do the thing".into(),
            source: "cli".into(),
            status,
            created_at: "2026-07-29T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            run_id: None,
            result: None,
            error: None,
        }
    }

    #[test]
    fn jobs_line_counts_terminal_over_total() {
        let jobs = vec![
            job("1", JobStatus::Done),
            job("2", JobStatus::Failed),
            job("3", JobStatus::Running),
            job("4", JobStatus::Queued),
            job("5", JobStatus::Queued),
        ];
        // 2 of 5 have reached a terminal state; one of those failed.
        let line = jobs_line("develop", &jobs);
        assert!(line.contains("fleet · develop"), "got: {line}");
        assert!(line.contains("job 2/5"), "got: {line}");
        assert!(line.contains("1 ⏵ running"), "got: {line}");
        assert!(line.contains("1 ✖ failed"), "got: {line}");
    }

    #[test]
    fn jobs_line_says_not_run_yet_when_there_are_none() {
        let line = jobs_line("develop", &[]);
        assert!(line.contains("not run yet"), "got: {line}");
        assert!(line.contains("mur fleet run develop"), "got: {line}");
    }

    #[test]
    fn jobs_line_omits_the_failed_clause_when_nothing_failed() {
        let line = jobs_line("develop", &[job("1", JobStatus::Done)]);
        assert!(!line.contains("failed"), "got: {line}");
    }

    use std::time::Instant;

    /// A fleet channel with one member. `create_for_fleet(fleet_name, router,
    /// members)` names the channel `fleet-<fleet_name>` itself — the rail must
    /// derive the same id from `--fleet dev`.
    fn seed_home() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        svc.create_for_fleet("dev", "mur", &["qa".to_string()])
            .unwrap();
        tmp
    }

    #[test]
    fn poll_reports_change_only_when_the_log_grows() {
        let tmp = seed_home();
        let now = Instant::now();
        let mut rail = FleetRail::start(tmp.path(), "dev");

        // First poll reads the (empty) channel and the (absent) job dir.
        assert!(rail.poll(tmp.path(), now), "first poll must produce a view");
        assert!(rail.view().members.is_empty());
        assert!(rail.view().jobs_line.contains("not run yet"));

        // Nothing changed → no work, no change reported.
        assert!(!rail.poll(tmp.path(), now));

        // A member acts → the next poll picks it up.
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        svc.append(
            "fleet-dev",
            ChannelActor::Agent { id: "qa".into() },
            EventKind::StateChange,
            serde_json::json!({"to": "working"}),
            None,
        )
        .unwrap();
        assert!(rail.poll(tmp.path(), now), "log grew → view must change");
        assert_eq!(rail.view().members.len(), 1);
        assert_eq!(rail.view().members[0].agent, "qa");
    }

    #[test]
    fn an_unreadable_channel_degrades_instead_of_failing() {
        let tmp = tempfile::TempDir::new().unwrap(); // no channel at all
        let mut rail = FleetRail::start(tmp.path(), "ghost");
        rail.poll(tmp.path(), Instant::now());
        assert!(rail.view().members.is_empty());
        // The rail says so on its own line; it never returns Err.
        assert!(rail.view().notice.is_some());
    }
}
