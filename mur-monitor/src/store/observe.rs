//! Append-only history (spec §冪等與事件紀錄) and the one write-back a check
//! cycle makes. `apply_cycle` is a single transaction guarded by the fence:
//! observation row, monitor columns, events, cycle finish, lease drop — all
//! or nothing, and nothing at all from a stale worker.
//!
//! `apply_cycle` is a hand-driven `BEGIN IMMEDIATE` / commit-or-rollback,
//! not `unchecked_transaction()` — same reasoning as `claim_due`, `release`
//! and `expire_leases` in `lease.rs` (see that module's doc comment for the
//! full explanation): it reads the fence and then writes, which is exactly
//! the read-then-write shape that a *deferred* transaction under
//! `journal_mode=WAL` can lose to `SQLITE_BUSY_SNAPSHOT` when a racing
//! writer commits in between — a failure `busy_timeout` does not retry.
//! Every exit path (no-such-monitor, stale fence, success) runs through
//! `lease::commit_or_rollback` so the connection never lingers inside an
//! open transaction; without that, a hand-driven `BEGIN IMMEDIATE` left open
//! on an early return would silently queue every later write instead of
//! autocommitting (the RAII rollback-on-drop that `unchecked_transaction()`
//! gives you does not exist here).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::lease::commit_or_rollback;
use super::{MonitorStore, parse_ts, ts};
use crate::adapter::Observation;
use crate::state::{MonitorState, Outcome};

#[derive(Debug, Clone)]
pub struct Event {
    pub kind: &'static str,
    pub payload: serde_json::Value,
    /// True = at most once per (monitor, cycle, kind) — the notable events.
    /// False = every time (`observed`, `lease_recovered`).
    pub dedup: bool,
    /// Overrides the dedup key that would otherwise be derived from `kind`
    /// alone. `None` is the common case (dedup key == `kind`). `Some` lets
    /// an event that can legitimately recur within the same (non-rotating)
    /// cycle — e.g. `monitor_unhealthy` re-announcing a later, higher
    /// streak — dedup on `kind:<episode>` instead of `kind`.
    pub dedup_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CycleUpdate {
    pub observation: Observation,
    pub observed_at: DateTime<Utc>,
    pub new_state: MonitorState,
    pub outcome: Outcome,
    pub next_check_at: DateTime<Utc>,
    pub pending_attempts: u32,
    pub unknown_streak: u32,
    pub last_progress_at: DateTime<Utc>,
    pub progress_token: Option<String>,
    pub stalled_since: Option<DateTime<Utc>>,
    pub soft_notified: bool,
    pub hard_reached: bool,
    /// Stamp `finished_at` + `terminal_outcome` on the current cycle.
    pub finish_cycle: bool,
    pub events: Vec<Event>,
}

#[derive(Debug, Clone)]
pub struct ObservationRow {
    pub cycle_id: String,
    pub observed_at: DateTime<Utc>,
    pub outcome: Outcome,
    pub progress_token: Option<String>,
    pub evidence: String,
    pub adapter_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub cycle_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

fn insert_event(
    conn: &rusqlite::Connection,
    id: &str,
    cycle_id: &str,
    event: &Event,
    now: DateTime<Utc>,
) -> rusqlite::Result<bool> {
    let dedup_key = if let Some(k) = &event.dedup_key {
        k.clone()
    } else if event.dedup {
        event.kind.to_string()
    } else {
        format!("{}:{}", event.kind, uuid::Uuid::now_v7())
    };
    let n = conn.execute(
        "INSERT OR IGNORE INTO monitor_events (monitor_id, cycle_id, kind, dedup_key, payload, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, cycle_id, event.kind, dedup_key, event.payload.to_string(), ts(now)],
    )?;
    Ok(n == 1)
}

impl MonitorStore {
    /// The one write-back a check cycle makes, guarded by `fence`: returns
    /// `false` and writes NOTHING — no observation row, no monitor column
    /// change, no event, no cycle finish, no lease release — when the
    /// monitor no longer exists or when `fence` is not the fence the row
    /// currently carries. A worker whose lease expired and was reclaimed by
    /// someone else must not overwrite the newer cycle (spec §租約).
    ///
    /// Both the "no such monitor" and "stale fence" exits fall through to
    /// `commit_or_rollback` with `Ok(false)`, which issues a plain `COMMIT`
    /// on a transaction that made no writes — releasing the write lock
    /// without ever touching a row. See the module doc for why an early
    /// `return` bypassing that tail would be wrong under a hand-driven
    /// `BEGIN IMMEDIATE`.
    pub fn apply_cycle(&self, id: &str, fence: i64, u: &CycleUpdate) -> Result<bool> {
        self.conn()
            .execute_batch("BEGIN IMMEDIATE")
            .context("begin apply_cycle transaction")?;

        let result = (|| -> Result<bool> {
            let current: Option<(i64, String)> = self
                .conn()
                .query_row(
                    "SELECT fence, cycle_id FROM monitors WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((cur_fence, cycle_id)) = current else {
                return Ok(false);
            };
            if cur_fence != fence {
                return Ok(false);
            }

            self.conn().execute(
                "INSERT INTO monitor_observations (monitor_id, cycle_id, fence, observed_at, outcome, progress_token, evidence, adapter_error) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    cycle_id,
                    fence,
                    ts(u.observed_at),
                    u.observation.outcome.as_str(),
                    u.observation.progress_token,
                    u.observation.evidence,
                    u.observation.adapter_error,
                ],
            )?;
            self.conn().execute(
                "UPDATE monitors SET state = ?1, outcome = ?2, next_check_at = ?3, last_checked_at = ?4, \
                 last_progress_at = ?5, progress_token = ?6, pending_attempts = ?7, unknown_streak = ?8, \
                 stalled_since = ?9, soft_notified = ?10, hard_reached = ?11, version = version + 1 \
                 WHERE id = ?12 AND fence = ?13",
                params![
                    u.new_state.as_str(),
                    u.outcome.as_str(),
                    ts(u.next_check_at),
                    ts(u.observed_at),
                    ts(u.last_progress_at),
                    u.progress_token,
                    u.pending_attempts as i64,
                    u.unknown_streak as i64,
                    u.stalled_since.map(ts),
                    u.soft_notified as i64,
                    u.hard_reached as i64,
                    id,
                    fence,
                ],
            )?;
            for e in &u.events {
                insert_event(self.conn(), id, &cycle_id, e, u.observed_at)?;
            }
            if u.finish_cycle {
                self.conn().execute(
                    "UPDATE monitor_cycles SET finished_at = ?1, terminal_outcome = ?2 WHERE id = ?3 AND finished_at IS NULL",
                    params![ts(u.observed_at), u.outcome.as_str(), cycle_id],
                )?;
            }
            // Deliberately part of the same transaction as the write-back
            // above (module doc / brief): a successful cycle write and the
            // lease release must never come apart.
            self.conn().execute(
                "DELETE FROM monitor_leases WHERE monitor_id = ?1 AND fence = ?2",
                params![id, fence],
            )?;
            Ok(true)
        })();

        commit_or_rollback(self.conn(), result, "apply_cycle")
    }

    pub fn append_event(
        &self,
        id: &str,
        cycle_id: &str,
        kind: &'static str,
        payload: serde_json::Value,
        dedup: bool,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let event = Event {
            kind,
            payload,
            dedup,
            dedup_key: None,
        };
        Ok(insert_event(self.conn(), id, cycle_id, &event, now)?)
    }

    /// Newest first — it is evidence.
    pub fn observations(&self, id: &str, limit: usize) -> Result<Vec<ObservationRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT cycle_id, observed_at, outcome, progress_token, evidence, adapter_error \
             FROM monitor_observations WHERE monitor_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![id, limit as i64], |r| {
            let outcome: String = r.get(2)?;
            Ok(ObservationRow {
                cycle_id: r.get(0)?,
                observed_at: parse_ts(&r.get::<_, String>(1)?),
                outcome: Outcome::parse(&outcome).unwrap_or(Outcome::Unknown),
                progress_token: r.get(3)?,
                evidence: r.get(4)?,
                adapter_error: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Oldest first — it is a history.
    pub fn events(&self, id: &str) -> Result<Vec<EventRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT cycle_id, kind, payload, created_at FROM monitor_events WHERE monitor_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([id], |r| {
            let payload: String = r.get(2)?;
            Ok(EventRow {
                cycle_id: r.get(0)?,
                kind: r.get(1)?,
                payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                created_at: parse_ts(&r.get::<_, String>(3)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Observation;
    use crate::store::tests::{spec, t0};
    use std::time::Duration;

    fn update(obs: Observation, state: MonitorState, now: DateTime<Utc>) -> CycleUpdate {
        CycleUpdate {
            outcome: obs.outcome,
            observation: obs,
            observed_at: now,
            new_state: state,
            next_check_at: now + chrono::Duration::seconds(30),
            pending_attempts: 1,
            unknown_streak: 0,
            last_progress_at: now,
            progress_token: Some("p1".into()),
            stalled_since: None,
            soft_notified: false,
            hard_reached: false,
            finish_cycle: state == MonitorState::Completed,
            events: vec![],
        }
    }

    #[test]
    fn apply_writes_observation_updates_row_and_releases_lease() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cl = s
            .claim_due(t0(), "w", Duration::from_secs(60), 1)
            .unwrap()
            .remove(0);
        let ok = s
            .apply_cycle(
                &c.id,
                cl.fence,
                &update(
                    Observation::pending("p1", "running"),
                    MonitorState::Sleeping,
                    t0(),
                ),
            )
            .unwrap();
        assert!(ok);
        let r = s.get(&c.id).unwrap().unwrap();
        assert_eq!(r.state, MonitorState::Sleeping);
        assert_eq!(r.outcome, Outcome::Pending);
        assert_eq!(r.pending_attempts, 1);
        assert_eq!(r.progress_token.as_deref(), Some("p1"));
        assert_eq!(r.last_checked_at, Some(t0()));
        assert_eq!(r.version, 2);
        assert!(s.lease_of(&c.id).unwrap().is_none());
        let obs = s.observations(&c.id, 10).unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].evidence, "running");
    }

    #[test]
    fn stale_fence_is_dropped_whole() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cl = s
            .claim_due(t0(), "w", Duration::from_secs(60), 1)
            .unwrap()
            .remove(0);
        let ok = s
            .apply_cycle(
                &c.id,
                cl.fence + 1,
                &update(
                    Observation::pending("p1", "late"),
                    MonitorState::Sleeping,
                    t0(),
                ),
            )
            .unwrap();
        assert!(!ok);
        assert!(
            s.observations(&c.id, 10).unwrap().is_empty(),
            "nothing from a stale worker lands"
        );
        assert_eq!(s.get(&c.id).unwrap().unwrap().state, MonitorState::Checking);
        assert!(
            s.lease_of(&c.id).unwrap().is_some(),
            "the live worker's lease is untouched"
        );
    }

    #[test]
    fn terminal_finishes_the_cycle() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cl = s
            .claim_due(t0(), "w", Duration::from_secs(60), 1)
            .unwrap()
            .remove(0);
        let mut u = update(
            Observation::terminal(Outcome::Succeeded, "done"),
            MonitorState::Completed,
            t0(),
        );
        u.events.push(Event {
            kind: "terminal",
            payload: serde_json::json!({"outcome": "succeeded"}),
            dedup: true,
            dedup_key: None,
        });
        assert!(s.apply_cycle(&c.id, cl.fence, &u).unwrap());
        let (finished, term): (Option<String>, Option<String>) = s
            .conn()
            .query_row(
                "SELECT finished_at, terminal_outcome FROM monitor_cycles WHERE id = ?1",
                [&cl.row.cycle_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(finished.is_some());
        assert_eq!(term.as_deref(), Some("succeeded"));
        let ev = s.events(&c.id).unwrap();
        assert_eq!(ev.iter().filter(|e| e.kind == "terminal").count(), 1);
    }

    #[test]
    fn dedup_events_fire_once_per_cycle_plain_events_every_time() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cyc = s.get(&c.id).unwrap().unwrap().cycle_id;
        assert!(
            s.append_event(&c.id, &cyc, "stalled", serde_json::json!({}), true, t0())
                .unwrap()
        );
        assert!(
            !s.append_event(&c.id, &cyc, "stalled", serde_json::json!({}), true, t0())
                .unwrap()
        );
        assert!(
            s.append_event(&c.id, &cyc, "observed", serde_json::json!({}), false, t0())
                .unwrap()
        );
        assert!(
            s.append_event(&c.id, &cyc, "observed", serde_json::json!({}), false, t0())
                .unwrap()
        );
        let kinds: Vec<_> = s
            .events(&c.id)
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, vec!["created", "stalled", "observed", "observed"]);
    }
}
