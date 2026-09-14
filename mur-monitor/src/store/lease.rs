//! Leases (spec §租約). A claim is one `BEGIN IMMEDIATE` transaction that
//! flips the row to `checking`, bumps its fence, and writes the lease row —
//! so two daemons opening the same file cannot both take a monitor. The
//! fence is monotonic per monitor; every later write-back must present the
//! fence it claimed under, and a stale one is refused (Task 6).
//!
//! `claim_due`, `release` and `expire_leases` are read-then-write and are
//! each driven as an explicit `BEGIN IMMEDIATE` / `COMMIT` rather than
//! rusqlite's safe `transaction()` wrapper — same reasoning and shape as
//! `MonitorStore::create` in `store/mod.rs` and `ChannelIndex::rebuild_from`
//! in `mur-channel/src/index.rs`:
//!
//! - These methods take `&self` (`MonitorStore` is shared behind `&self` by
//!   every caller), so the safe wrapper — which needs `&mut Connection` — is
//!   not available; the transaction is driven by hand, with `ROLLBACK` on
//!   every error path before the error is propagated.
//! - `BEGIN IMMEDIATE` (not the default deferred behaviour of
//!   `unchecked_transaction()`) takes the write lock up front, before the
//!   first `SELECT` runs. Under `journal_mode=WAL`, a *deferred* transaction
//!   fixes its read snapshot at that `SELECT`; if another connection commits
//!   a conflicting change afterward, this transaction's own write then fails
//!   with `SQLITE_BUSY_SNAPSHOT` — a variant `busy_timeout` does not retry,
//!   because a stale-snapshot writer cannot be resolved without restarting
//!   the whole transaction. `BEGIN IMMEDIATE` avoids the snapshot ever going
//!   stale: it acquires the write lock (waiting on `busy_timeout` like any
//!   other writer) before the read, so the read always sees the latest
//!   committed state and no concurrent writer can slip a change in
//!   underneath it. `claim_due` racing a second daemon for the same due
//!   monitor is exactly this scenario.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use super::{MONITOR_COLS, MonitorRow, MonitorStore, parse_ts, row_to_monitor, ts};
use crate::state::MonitorState;

#[derive(Debug, Clone)]
pub struct Claimed {
    pub row: MonitorRow,
    pub fence: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub owner: String,
    pub expires_at: DateTime<Utc>,
    pub fence: i64,
}

fn expiry(now: DateTime<Utc>, lease: Duration) -> String {
    ts(now + chrono::Duration::from_std(lease).unwrap_or_else(|_| chrono::Duration::seconds(60)))
}

/// SQL `IN (...)` list of the claimable states, derived from
/// `MonitorState::is_claimable` rather than hardcoded, so this stays in
/// lockstep with the state model in `state.rs`.
fn claimable_states_sql() -> String {
    MonitorState::ALL
        .iter()
        .copied()
        .filter(|s| s.is_claimable())
        .map(|s| format!("'{}'", s.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Shared `COMMIT`-on-`Ok` / `ROLLBACK`-on-`Err` tail for the three
/// `BEGIN IMMEDIATE` methods below (see module doc for why they cannot use
/// rusqlite's safe `transaction()` wrapper).
fn commit_or_rollback<T>(conn: &Connection, result: Result<T>, what: &str) -> Result<T> {
    match result {
        Ok(v) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(v),
            Err(e) => {
                // COMMIT itself failed (e.g. disk full at commit time).
                // Without this, `?` would escape with the transaction still
                // open, and `conn` would silently queue every later
                // statement inside it instead of autocommitting. Roll back
                // before propagating, same as the Err(e) arm below; a
                // rollback that itself fails must not mask the original
                // COMMIT error.
                let _ = conn.execute_batch("ROLLBACK");
                Err(e).with_context(|| format!("commit {what} transaction"))
            }
        },
        Err(e) => {
            // Best-effort: if the rollback itself fails, the original error
            // is still the one worth surfacing, not the rollback failure.
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

impl MonitorStore {
    /// Atomically claim up to `max` due monitors. Due = claimable state,
    /// `next_check_at <= now`, and no live lease. `max` is the per-tick
    /// throttle (spec §daemon 恢復: bounded work queue) — it genuinely bounds
    /// how many rows this call touches, via `LIMIT`.
    pub fn claim_due(
        &self,
        now: DateTime<Utc>,
        owner: &str,
        lease: Duration,
        max: usize,
    ) -> Result<Vec<Claimed>> {
        self.conn()
            .execute_batch("BEGIN IMMEDIATE")
            .context("begin claim_due transaction")?;

        let result = (|| -> Result<Vec<Claimed>> {
            let ids: Vec<String> = {
                let sql = format!(
                    "SELECT m.id FROM monitors m
                     LEFT JOIN monitor_leases l ON l.monitor_id = m.id
                     WHERE m.state IN ({states})
                       AND m.next_check_at <= ?1
                       AND (l.monitor_id IS NULL OR l.expires_at <= ?1)
                     ORDER BY m.next_check_at ASC
                     LIMIT ?2",
                    states = claimable_states_sql()
                );
                let mut stmt = self.conn().prepare(&sql)?;
                let rows = stmt.query_map(params![ts(now), max as i64], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                self.conn().execute(
                    "UPDATE monitors SET state = ?1, fence = fence + 1 WHERE id = ?2",
                    params![MonitorState::Checking.as_str(), id],
                )?;
                let row = self.conn().query_row(
                    &format!("SELECT {MONITOR_COLS} FROM monitors WHERE id = ?1"),
                    [&id],
                    row_to_monitor,
                )?;
                self.conn().execute(
                    "INSERT OR REPLACE INTO monitor_leases (monitor_id, owner, expires_at, fence) VALUES (?1, ?2, ?3, ?4)",
                    params![id, owner, expiry(now, lease), row.fence],
                )?;
                let fence = row.fence;
                out.push(Claimed { row, fence });
            }
            Ok(out)
        })();

        commit_or_rollback(self.conn(), result, "claim_due")
    }

    /// Extend the lease during a long query. False = fence is stale (someone
    /// else holds this monitor now); the caller must stop and discard. A
    /// single `UPDATE ... WHERE fence = ?` is already atomic, so no explicit
    /// transaction is needed here.
    pub fn heartbeat(
        &self,
        id: &str,
        fence: i64,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE monitor_leases SET expires_at = ?1 WHERE monitor_id = ?2 AND fence = ?3",
            params![expiry(now, lease), id, fence],
        )?;
        Ok(n == 1)
    }

    /// Drop the lease and put the monitor back into `back_to` without
    /// recording an observation — the no-adapter path. `apply_cycle` (Task 6)
    /// releases as part of its own transaction. False = fence is stale;
    /// nothing is changed.
    pub fn release(&self, id: &str, fence: i64, back_to: MonitorState) -> Result<bool> {
        self.conn()
            .execute_batch("BEGIN IMMEDIATE")
            .context("begin release transaction")?;

        let result = (|| -> Result<bool> {
            let n = self.conn().execute(
                "DELETE FROM monitor_leases WHERE monitor_id = ?1 AND fence = ?2",
                params![id, fence],
            )?;
            if n == 1 {
                self.conn().execute(
                    "UPDATE monitors SET state = ?1 WHERE id = ?2 AND fence = ?3",
                    params![back_to.as_str(), id, fence],
                )?;
            }
            Ok(n == 1)
        })();

        commit_or_rollback(self.conn(), result, "release")
    }

    /// Recovery (spec §daemon 恢復 step 2): every expired lease is dropped,
    /// its monitor returned to `active` if it was mid-check, and a
    /// `lease_recovered` event appended. Returns the affected monitor ids.
    pub fn expire_leases(&self, now: DateTime<Utc>) -> Result<Vec<String>> {
        self.conn()
            .execute_batch("BEGIN IMMEDIATE")
            .context("begin expire_leases transaction")?;

        let result = (|| -> Result<Vec<String>> {
            let expired: Vec<(String, String, i64)> = {
                let mut stmt = self.conn().prepare(
                    "SELECT l.monitor_id, l.owner, l.fence FROM monitor_leases l WHERE l.expires_at <= ?1",
                )?;
                let rows = stmt.query_map([ts(now)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            let mut ids = Vec::with_capacity(expired.len());
            for (id, owner, fence) in expired {
                self.conn()
                    .execute("DELETE FROM monitor_leases WHERE monitor_id = ?1", [&id])?;
                self.conn().execute(
                    "UPDATE monitors SET state = ?1 WHERE id = ?2 AND state = ?3",
                    params![
                        MonitorState::Active.as_str(),
                        id,
                        MonitorState::Checking.as_str()
                    ],
                )?;
                let cycle_id: String = self.conn().query_row(
                    "SELECT cycle_id FROM monitors WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )?;
                self.conn().execute(
                    "INSERT OR IGNORE INTO monitor_events (monitor_id, cycle_id, kind, dedup_key, payload, created_at) \
                     VALUES (?1, ?2, 'lease_recovered', ?3, ?4, ?5)",
                    params![
                        id,
                        cycle_id,
                        format!("lease_recovered:{fence}"),
                        serde_json::json!({ "owner": owner, "fence": fence }).to_string(),
                        ts(now)
                    ],
                )?;
                ids.push(id);
            }
            Ok(ids)
        })();

        commit_or_rollback(self.conn(), result, "expire_leases")
    }

    pub fn lease_of(&self, id: &str) -> Result<Option<Lease>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT owner, expires_at, fence FROM monitor_leases WHERE monitor_id = ?1",
                [id],
                |r| {
                    Ok(Lease {
                        owner: r.get(0)?,
                        expires_at: parse_ts(&r.get::<_, String>(1)?),
                        fence: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{spec, t0};
    use std::time::Duration;

    const LEASE: Duration = Duration::from_secs(120);

    #[test]
    fn two_workers_one_wins() {
        let d = tempfile::tempdir().unwrap();
        let a = MonitorStore::open(d.path()).unwrap();
        let b = MonitorStore::open(d.path()).unwrap();
        a.create(&spec("k"), t0(), None).unwrap();
        let got_a = a.claim_due(t0(), "wa", LEASE, 10).unwrap();
        let got_b = b.claim_due(t0(), "wb", LEASE, 10).unwrap();
        assert_eq!(got_a.len(), 1);
        assert_eq!(got_b.len(), 0, "monitor is `checking` and leased");
        assert_eq!(got_a[0].fence, 1);
        assert_eq!(got_a[0].row.state, MonitorState::Checking);
        let l = a.lease_of(&got_a[0].row.id).unwrap().unwrap();
        assert_eq!(l.owner, "wa");
        assert_eq!(l.expires_at, t0() + chrono::Duration::seconds(120));
    }

    #[test]
    fn not_due_is_not_claimed() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        s.conn()
            .execute(
                "UPDATE monitors SET next_check_at = ?1 WHERE id = ?2",
                rusqlite::params![ts(t0() + chrono::Duration::seconds(30)), c.id],
            )
            .unwrap();
        assert!(s.claim_due(t0(), "w", LEASE, 10).unwrap().is_empty());
        assert_eq!(
            s.claim_due(t0() + chrono::Duration::seconds(30), "w", LEASE, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn expired_lease_is_recovered_and_reclaimed_with_a_higher_fence() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let first = s.claim_due(t0(), "dead-worker", LEASE, 10).unwrap();
        assert_eq!(first[0].fence, 1);
        let later = t0() + chrono::Duration::seconds(121);
        assert!(
            s.claim_due(later, "w2", LEASE, 10).unwrap().is_empty(),
            "still `checking` until recovered"
        );
        assert_eq!(s.expire_leases(later).unwrap(), vec![c.id.clone()]);
        assert_eq!(s.get(&c.id).unwrap().unwrap().state, MonitorState::Active);
        let second = s.claim_due(later, "w2", LEASE, 10).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].fence, 2);
    }

    #[test]
    fn stale_fence_cannot_heartbeat_or_release() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let c = s.create(&spec("k"), t0(), None).unwrap();
        let cl = s.claim_due(t0(), "w", LEASE, 10).unwrap();
        assert!(!s.heartbeat(&c.id, cl[0].fence - 1, t0(), LEASE).unwrap());
        assert!(
            s.heartbeat(
                &c.id,
                cl[0].fence,
                t0() + chrono::Duration::seconds(60),
                LEASE
            )
            .unwrap()
        );
        assert_eq!(
            s.lease_of(&c.id).unwrap().unwrap().expires_at,
            t0() + chrono::Duration::seconds(180)
        );
        assert!(!s.release(&c.id, 99, MonitorState::Sleeping).unwrap());
        assert!(
            s.release(&c.id, cl[0].fence, MonitorState::Sleeping)
                .unwrap()
        );
        assert!(s.lease_of(&c.id).unwrap().is_none());
        assert_eq!(s.get(&c.id).unwrap().unwrap().state, MonitorState::Sleeping);
    }

    #[test]
    fn max_bounds_a_thundering_herd() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        for i in 0..20 {
            s.create(&spec(&format!("k{i}")), t0(), None).unwrap();
        }
        assert_eq!(s.claim_due(t0(), "w", LEASE, 8).unwrap().len(), 8);
        assert_eq!(s.claim_due(t0(), "w", LEASE, 8).unwrap().len(), 8);
        assert_eq!(s.claim_due(t0(), "w", LEASE, 8).unwrap().len(), 4);
    }

    /// The five tests above only ever exercise one `MonitorStore` connection
    /// at a time (or two, called sequentially) — none of them prove the
    /// `BEGIN IMMEDIATE` claim under an actual race. Mirrors
    /// `store::tests::concurrent_create_on_one_key_never_errors_and_never_duplicates`
    /// in `store/mod.rs` (Task 4's race test): one `MonitorStore` per thread
    /// against the same on-disk file, lined up with a `Barrier` so their
    /// `claim_due` calls on the single due monitor genuinely overlap.
    #[test]
    fn concurrent_claim_due_never_double_claims() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let s0 = MonitorStore::open(&dir).unwrap();
        let c = s0.create(&spec("race"), t0(), None).unwrap();

        const N: usize = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|i| {
                let dir = dir.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let s = MonitorStore::open(&dir).unwrap();
                    barrier.wait();
                    s.claim_due(t0(), &format!("w{i}"), LEASE, 10)
                })
            })
            .collect();
        let results: Vec<Result<Vec<Claimed>>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        let mut total_claimed = 0;
        let mut fences = Vec::new();
        for r in &results {
            let v = r
                .as_ref()
                .expect("claim_due must never error under a racing claim");
            total_claimed += v.len();
            fences.extend(v.iter().map(|c| c.fence));
        }
        assert_eq!(
            total_claimed, 1,
            "exactly one worker claims the single due monitor"
        );
        assert_eq!(fences, vec![1]);
        assert_eq!(s0.lease_of(&c.id).unwrap().unwrap().fence, 1);
    }
}
