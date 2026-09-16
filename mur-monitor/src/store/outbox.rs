//! The `monitor_registration_outbox` table (spec §自動註冊邊界 clause 2):
//! recoverable parking for a `MonitorSpec` whose immediate `create()` failed
//! even though the work it describes is already running. This is the "not
//! on the floor" half of `register_or_outbox` (`crate::register`) — a spec
//! that lands here is neither monitored nor lost.
//!
//! Table already exists (`store/mod.rs`'s `migrate()`): `id`, `spec_json`,
//! `created_at`, `attempts`, `last_error`. No schema change, no
//! `SCHEMA_USER_VERSION` bump.
//!
//! Every write below is a single SQL statement (`INSERT`, `DELETE`, or an
//! `UPDATE` whose new value SQLite computes itself), so — same reasoning as
//! `store/action.rs`'s module doc — none of it needs an explicit
//! `BEGIN IMMEDIATE` transaction: a lone statement is already atomic under
//! SQLite's default autocommit, and nothing here reads a value in one
//! statement to act on it in a second.

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::{MonitorStore, parse_ts, ts};
use crate::spec::MonitorSpec;

#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id: String,
    pub spec: MonitorSpec,
    pub created_at: DateTime<Utc>,
    pub attempts: u32,
    pub last_error: Option<String>,
    /// When the last retry was attempted. `None` for a row that has never
    /// been retried — and for rows written before this column existed, which
    /// read as "never attempted" and become due at once. That is the right
    /// answer for a row that was already waiting when the upgrade landed.
    pub last_attempt_at: Option<DateTime<Utc>>,
}

impl MonitorStore {
    /// Park a spec whose immediate registration failed. Returns the outbox
    /// row id — NOT a monitor id; the caller must not treat it as one.
    pub fn outbox_enqueue(&self, spec: &MonitorSpec, now: DateTime<Utc>) -> Result<String> {
        let id = uuid::Uuid::now_v7().to_string();
        self.conn().execute(
            "INSERT INTO monitor_registration_outbox (id, spec_json, created_at, attempts, last_error) \
             VALUES (?1, ?2, ?3, 0, NULL)",
            rusqlite::params![id, serde_json::to_string(spec)?, ts(now)],
        )?;
        Ok(id)
    }

    /// Rows still awaiting a retry, oldest first. `now` is part of the
    /// interface a future drain dials (mirrors `pending_actions` in
    /// `store/action.rs`), kept here for forward compatibility even though
    /// this table carries no retry-delay column yet — there is nowhere to
    /// persist a computed "not before" time without a schema change, which
    /// this task is explicitly told not to make. A caller wanting backoff
    /// between retries computes it from `attempts` via `crate::backoff` at
    /// the call site instead.
    /// Ordered by when each row was last tried, not when it was created, so
    /// a handful of permanently-failing rows cannot hold the oldest-first
    /// window and starve rows queued after them.
    pub fn outbox_due(&self, now: DateTime<Utc>, max: usize) -> Result<Vec<OutboxRow>> {
        let _ = now;
        let mut stmt = self.conn().prepare(
            "SELECT id, spec_json, created_at, attempts, last_error, last_attempt_at \
             FROM monitor_registration_outbox \
             ORDER BY COALESCE(last_attempt_at, created_at) ASC, id ASC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![max as i64], row_to_outbox)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
            .map_err(Into::into)
    }

    /// Drop a row once its retry lands a real monitor, or it is given up on.
    pub fn outbox_drop(&self, id: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM monitor_registration_outbox WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    /// Record a failed retry: bumps `attempts` and stores the error,
    /// redacted first — same rule as `store/action.rs`'s stored action
    /// results, a secret must never reach history.
    pub fn outbox_record_failure(&self, id: &str, error: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn().execute(
            "UPDATE monitor_registration_outbox SET attempts = attempts + 1, last_error = ?1, \
             last_attempt_at = ?2 WHERE id = ?3",
            rusqlite::params![
                mur_common::redact::redact_secrets(error),
                crate::store::ts(now),
                id
            ],
        )?;
        Ok(())
    }
}

/// `None` for a row whose `spec_json` no longer parses — forward
/// compatibility, not a crash: a newer build's spec shape must not panic an
/// older one draining the outbox.
fn row_to_outbox(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<OutboxRow>> {
    let spec_json: String = r.get(1)?;
    let Ok(spec) = serde_json::from_str::<MonitorSpec>(&spec_json) else {
        return Ok(None);
    };
    Ok(Some(OutboxRow {
        id: r.get(0)?,
        spec,
        created_at: parse_ts(&r.get::<_, String>(2)?),
        attempts: r.get::<_, i64>(3)? as u32,
        last_error: r.get(4)?,
        last_attempt_at: r.get::<_, Option<String>>(5)?.map(|t| parse_ts(&t)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{spec, t0};

    #[test]
    fn enqueue_then_due_returns_the_row_with_its_spec() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let sp = spec("k1");
        let id = s.outbox_enqueue(&sp, t0()).unwrap();
        let due = s.outbox_due(t0(), 10).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, id);
        assert_eq!(due[0].spec.idempotency_key, sp.idempotency_key);
        assert_eq!(due[0].attempts, 0);
        assert!(due[0].last_error.is_none());
    }

    #[test]
    fn record_failure_counts_up_and_redacts() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.outbox_enqueue(&spec("k1"), t0()).unwrap();
        let token = format!("ghp_{}", "A".repeat(36));
        s.outbox_record_failure(&id, &format!("token={token} denied"), t0())
            .unwrap();
        let due = s.outbox_due(t0(), 10).unwrap();
        assert_eq!(due[0].attempts, 1);
        let err = due[0].last_error.as_deref().unwrap();
        assert!(
            !err.contains(&token),
            "secret must not reach history: {err:?}"
        );
        assert!(err.contains("denied"));
        s.outbox_record_failure(&id, "denied again", t0()).unwrap();
        assert_eq!(s.outbox_due(t0(), 10).unwrap()[0].attempts, 2);
    }

    #[test]
    fn drop_removes_the_row() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.outbox_enqueue(&spec("k1"), t0()).unwrap();
        assert_eq!(s.outbox_due(t0(), 10).unwrap().len(), 1);
        s.outbox_drop(&id).unwrap();
        assert!(s.outbox_due(t0(), 10).unwrap().is_empty());
    }

    #[test]
    fn due_is_bounded_by_max_and_ordered_oldest_first() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let a = s.outbox_enqueue(&spec("a"), t0()).unwrap();
        let _b = s
            .outbox_enqueue(&spec("b"), t0() + chrono::Duration::seconds(1))
            .unwrap();
        let due = s.outbox_due(t0(), 1).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, a);
    }
}
