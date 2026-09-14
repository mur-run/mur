//! SQLite persistence for monitors (spec §SQLite 資料模型). One file,
//! `<mur_home>/monitor/monitors.db`, WAL + busy_timeout exactly as
//! `mur-channel/src/index.rs` does — the CLI, the daemon and (plan-2) an
//! agent runtime open independent connections to it.
//!
//! Split: this file owns the schema and the monitor row; `lease.rs` owns
//! claim/heartbeat/expiry; `observe.rs` owns observations, events and the
//! write-back of one check cycle.

mod lease;
mod observe;

pub use lease::{Claimed, Lease};
pub use observe::{CycleUpdate, Event, EventRow, ObservationRow};

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::spec::{MonitorSpec, SourceType};
use crate::state::{MonitorState, Outcome};

pub const DB_FILE: &str = "monitors.db";
/// Bumped when a migration below changes a column's meaning.
pub const SCHEMA_USER_VERSION: i64 = 1;

pub fn db_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("monitor")
}

pub struct MonitorStore {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct MonitorRow {
    pub id: String,
    pub name: String,
    pub spec: MonitorSpec,
    pub state: MonitorState,
    pub outcome: Outcome,
    pub source_type: SourceType,
    pub reference: String,
    pub idempotency_key: String,
    pub created_at: DateTime<Utc>,
    pub work_started_at: DateTime<Utc>,
    pub next_check_at: DateTime<Utc>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_progress_at: DateTime<Utc>,
    pub progress_token: Option<String>,
    pub pending_attempts: u32,
    pub unknown_streak: u32,
    pub remediation_attempts: u32,
    pub cycle_id: String,
    pub stalled_since: Option<DateTime<Utc>>,
    pub soft_notified: bool,
    pub hard_reached: bool,
    pub fence: i64,
    pub version: i64,
}

#[derive(Debug, Clone)]
pub struct Created {
    pub id: String,
    pub existing: bool,
    pub next_check_at: DateTime<Utc>,
}

#[derive(Debug, Default, Clone)]
pub struct ListFilter {
    pub state: Option<MonitorState>,
    pub include_completed: bool,
}

pub(crate) fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::<Utc>::UNIX_EPOCH)
}

pub(crate) const MONITOR_COLS: &str = "id, name, spec_json, state, outcome, source_type, reference, idempotency_key, \
     created_at, work_started_at, next_check_at, last_checked_at, last_progress_at, progress_token, \
     pending_attempts, unknown_streak, remediation_attempts, cycle_id, stalled_since, soft_notified, \
     hard_reached, fence, version";

fn conv<E: std::error::Error + Send + Sync + 'static>(e: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
}

pub(crate) fn row_to_monitor(r: &rusqlite::Row<'_>) -> rusqlite::Result<MonitorRow> {
    let spec_json: String = r.get(2)?;
    let state: String = r.get(3)?;
    let outcome: String = r.get(4)?;
    let source_type: String = r.get(5)?;
    let last_checked: Option<String> = r.get(11)?;
    let stalled: Option<String> = r.get(18)?;
    Ok(MonitorRow {
        id: r.get(0)?,
        name: r.get(1)?,
        spec: serde_json::from_str(&spec_json).map_err(conv)?,
        state: MonitorState::parse(&state)
            .ok_or_else(|| conv(std::io::Error::other(format!("bad state {state}"))))?,
        outcome: Outcome::parse(&outcome)
            .ok_or_else(|| conv(std::io::Error::other(format!("bad outcome {outcome}"))))?,
        source_type: SourceType::parse(&source_type)
            .ok_or_else(|| conv(std::io::Error::other(format!("bad source {source_type}"))))?,
        reference: r.get(6)?,
        idempotency_key: r.get(7)?,
        created_at: parse_ts(&r.get::<_, String>(8)?),
        work_started_at: parse_ts(&r.get::<_, String>(9)?),
        next_check_at: parse_ts(&r.get::<_, String>(10)?),
        last_checked_at: last_checked.as_deref().map(parse_ts),
        last_progress_at: parse_ts(&r.get::<_, String>(12)?),
        progress_token: r.get(13)?,
        pending_attempts: r.get::<_, i64>(14)? as u32,
        unknown_streak: r.get::<_, i64>(15)? as u32,
        remediation_attempts: r.get::<_, i64>(16)? as u32,
        cycle_id: r.get(17)?,
        stalled_since: stalled.as_deref().map(parse_ts),
        soft_notified: r.get::<_, i64>(19)? != 0,
        hard_reached: r.get::<_, i64>(20)? != 0,
        fence: r.get(21)?,
        version: r.get(22)?,
    })
}

impl MonitorStore {
    pub fn open(mur_home: &Path) -> Result<Self> {
        let dir = db_dir(mur_home);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let conn = Connection::open(dir.join(DB_FILE)).context("open monitors.db")?;
        // busy_timeout BEFORE journal_mode — see mur-channel/src/index.rs for why:
        // switching journal mode takes a lock on the database, and when several
        // processes open the DB concurrently that lock is contended — without a
        // busy_timeout already in effect the WAL switch itself fails immediately
        // with SQLITE_BUSY. Setting the timeout first makes the contended
        // connection wait and retry instead of failing.
        conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;")
            .context("configure monitors.db pragmas")?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS monitors (
                id                   TEXT PRIMARY KEY,
                name                 TEXT NOT NULL,
                spec_json            TEXT NOT NULL,
                state                TEXT NOT NULL,
                outcome              TEXT NOT NULL,
                source_type          TEXT NOT NULL,
                reference            TEXT NOT NULL,
                idempotency_key      TEXT NOT NULL,
                created_at           TEXT NOT NULL,
                work_started_at      TEXT NOT NULL,
                next_check_at        TEXT NOT NULL,
                last_checked_at      TEXT,
                last_progress_at     TEXT NOT NULL,
                progress_token       TEXT,
                pending_attempts     INTEGER NOT NULL DEFAULT 0,
                unknown_streak       INTEGER NOT NULL DEFAULT 0,
                remediation_attempts INTEGER NOT NULL DEFAULT 0,
                cycle_id             TEXT NOT NULL,
                stalled_since        TEXT,
                soft_notified        INTEGER NOT NULL DEFAULT 0,
                hard_reached         INTEGER NOT NULL DEFAULT 0,
                fence                INTEGER NOT NULL DEFAULT 0,
                version              INTEGER NOT NULL DEFAULT 1
            );
            CREATE INDEX IF NOT EXISTS idx_monitors_due ON monitors(state, next_check_at);
            -- rule 6: unique among monitors that are not finished
            CREATE UNIQUE INDEX IF NOT EXISTS idx_monitors_open_key
                ON monitors(idempotency_key) WHERE state != 'completed';

            CREATE TABLE IF NOT EXISTS monitor_cycles (
                id               TEXT PRIMARY KEY,
                monitor_id       TEXT NOT NULL,
                parent_cycle_id  TEXT,
                reference        TEXT NOT NULL,
                started_at       TEXT NOT NULL,
                finished_at      TEXT,
                terminal_outcome TEXT
            );
            CREATE TABLE IF NOT EXISTS monitor_leases (
                monitor_id TEXT PRIMARY KEY,
                owner      TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                fence      INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS monitor_observations (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                monitor_id     TEXT NOT NULL,
                cycle_id       TEXT NOT NULL,
                fence          INTEGER NOT NULL,
                observed_at    TEXT NOT NULL,
                outcome        TEXT NOT NULL,
                progress_token TEXT,
                evidence       TEXT NOT NULL,
                adapter_error  TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_obs_monitor ON monitor_observations(monitor_id, id DESC);
            CREATE TABLE IF NOT EXISTS monitor_events (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                monitor_id TEXT NOT NULL,
                cycle_id   TEXT NOT NULL,
                kind       TEXT NOT NULL,
                dedup_key  TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(monitor_id, cycle_id, dedup_key)
            );
            -- plan-2 tables, created now so later migrations stay additive
            CREATE TABLE IF NOT EXISTS monitor_actions (
                action_key  TEXT PRIMARY KEY,
                monitor_id  TEXT NOT NULL,
                cycle_id    TEXT NOT NULL,
                risk        TEXT NOT NULL,
                approval_id TEXT,
                state       TEXT NOT NULL,
                attempt     INTEGER NOT NULL DEFAULT 0,
                result      TEXT,
                created_at  TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS monitor_notifications (
                event_key      TEXT PRIMARY KEY,
                monitor_id     TEXT NOT NULL,
                channel        TEXT NOT NULL,
                delivery_state TEXT NOT NULL,
                updated_at     TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS monitor_registration_outbox (
                id         TEXT PRIMARY KEY,
                spec_json  TEXT NOT NULL,
                created_at TEXT NOT NULL,
                attempts   INTEGER NOT NULL DEFAULT 0,
                last_error TEXT
            );",
        )?;
        self.conn
            .pragma_update(None, "user_version", SCHEMA_USER_VERSION)?;
        Ok(())
    }

    /// Rule 6: a second `create` with the same key while the first is still
    /// open returns the first monitor, never a duplicate — including across
    /// the independent connections the CLI, the daemon and (plan-2) an
    /// agent runtime each open onto this same file.
    ///
    /// Driven as an explicit `BEGIN IMMEDIATE` / `COMMIT` rather than
    /// rusqlite's safe `transaction()` wrapper, for two reasons (same shape
    /// as `ChannelIndex::rebuild_from` in `mur-channel/src/index.rs`):
    ///
    /// - `transaction()` needs `&mut Connection`; this method takes `&self`
    ///   (`MonitorStore` is shared behind `&self` by every caller), so the
    ///   safe wrapper is not available and the transaction is driven by
    ///   hand, with `ROLLBACK` on every error path before the error is
    ///   propagated.
    /// - `BEGIN IMMEDIATE` (not the default `BEGIN`/deferred behaviour of
    ///   `unchecked_transaction()`) takes the write lock up front, before
    ///   the existence `SELECT` runs. Under `journal_mode=WAL`, a *deferred*
    ///   transaction fixes its read snapshot at that `SELECT`; if another
    ///   connection commits a row with the same `idempotency_key` afterward,
    ///   this transaction's own `INSERT` then fails with
    ///   `SQLITE_BUSY_SNAPSHOT` — a variant `busy_timeout` does not retry,
    ///   because a stale-snapshot writer cannot be resolved without
    ///   restarting the whole transaction. `BEGIN IMMEDIATE` avoids the
    ///   snapshot ever going stale: it acquires the write lock (waiting on
    ///   `busy_timeout` like any other writer) before the `SELECT`, so the
    ///   `SELECT` always sees the latest committed state and no concurrent
    ///   writer can slip a row in underneath it.
    pub fn create(
        &self,
        spec: &MonitorSpec,
        now: DateTime<Utc>,
        work_started_at: Option<DateTime<Utc>>,
    ) -> Result<Created> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .context("begin create transaction")?;

        let result = (|| -> Result<Created> {
            let existing: Option<(String, String)> = self
                .conn
                .query_row(
                    "SELECT id, next_check_at FROM monitors WHERE idempotency_key = ?1 AND state != 'completed'",
                    [&spec.idempotency_key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, next)) = existing {
                return Ok(Created {
                    id,
                    existing: true,
                    next_check_at: parse_ts(&next),
                });
            }
            let id = uuid::Uuid::now_v7().to_string();
            let cycle_id = uuid::Uuid::now_v7().to_string();
            let started = work_started_at.unwrap_or(now);
            self.conn.execute(
                &format!(
                    "INSERT INTO monitors ({MONITOR_COLS}) VALUES \
                     (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, NULL, 0, 0, 0, ?13, NULL, 0, 0, 0, 1)"
                ),
                params![
                    id,
                    spec.name,
                    serde_json::to_string(spec)?,
                    MonitorState::Active.as_str(),
                    Outcome::Pending.as_str(),
                    spec.source.r#type.as_str(),
                    spec.source.reference,
                    spec.idempotency_key,
                    ts(now),
                    ts(started),
                    ts(now),
                    ts(started),
                    cycle_id,
                ],
            )?;
            self.conn.execute(
                "INSERT INTO monitor_cycles (id, monitor_id, parent_cycle_id, reference, started_at) VALUES (?1, ?2, NULL, ?3, ?4)",
                params![cycle_id, id, spec.source.reference, ts(started)],
            )?;
            self.conn.execute(
                "INSERT INTO monitor_events (monitor_id, cycle_id, kind, dedup_key, payload, created_at) VALUES (?1, ?2, 'created', 'created', ?3, ?4)",
                params![
                    id,
                    cycle_id,
                    serde_json::json!({
                        "schema_version": spec.schema_version,
                        "actor": spec.created_by.actor,
                        "reason": spec.created_by.reason,
                        "originating_run_id": spec.created_by.originating_run_id,
                    })
                    .to_string(),
                    ts(now)
                ],
            )?;
            Ok(Created {
                id,
                existing: false,
                next_check_at: now,
            })
        })();

        match result {
            Ok(created) => match self.conn.execute_batch("COMMIT") {
                Ok(()) => Ok(created),
                Err(e) => {
                    // COMMIT itself failed (e.g. disk full at commit time).
                    // Without this, `?` would escape with the transaction
                    // still open, and `self.conn` would silently queue
                    // every later statement inside it instead of
                    // autocommitting. Roll back before propagating, same
                    // as the Err(e) arm below; a rollback that itself
                    // fails must not mask the original COMMIT error.
                    let _ = self.conn.execute_batch("ROLLBACK");
                    Err(e).context("commit create transaction")
                }
            },
            Err(e) => {
                // Best-effort: if the rollback itself fails, the original
                // error is still the one worth surfacing, not the
                // rollback failure.
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn get(&self, id: &str) -> Result<Option<MonitorRow>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {MONITOR_COLS} FROM monitors WHERE id = ?1"),
                [id],
                row_to_monitor,
            )
            .optional()?)
    }

    pub fn list(&self, f: &ListFilter) -> Result<Vec<MonitorRow>> {
        let mut sql = format!("SELECT {MONITOR_COLS} FROM monitors WHERE 1=1");
        let mut args: Vec<String> = Vec::new();
        if let Some(s) = f.state {
            sql.push_str(" AND state = ?1");
            args.push(s.as_str().to_string());
        } else if !f.include_completed {
            sql.push_str(" AND state != 'completed'");
        }
        sql.push_str(" ORDER BY next_check_at ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), row_to_monitor)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Direct state write for CLI verbs (`cancel`) — bumps `version`, leaves
    /// the fence alone (no lease is involved).
    pub fn set_state(&self, id: &str, state: MonitorState, now: DateTime<Utc>) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE monitors SET state = ?1, version = version + 1, last_checked_at = COALESCE(last_checked_at, ?2) WHERE id = ?3",
            params![state.as_str(), ts(now), id],
        )?;
        Ok(n == 1)
    }

    /// `mur monitor retry`: only an `exhausted` monitor comes back. Clears
    /// the unknown streak (a fresh look), keeps the remediation budget
    /// unless the caller explicitly resets it (spec §CLI).
    pub fn reactivate(&self, id: &str, now: DateTime<Utc>, reset_budget: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE monitors SET state = 'active', next_check_at = ?1, unknown_streak = 0, \
             remediation_attempts = CASE WHEN ?2 THEN 0 ELSE remediation_attempts END, \
             version = version + 1 WHERE id = ?3 AND state = 'exhausted'",
            params![ts(now), reset_budget, id],
        )?;
        Ok(n == 1)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone;

    pub(crate) fn spec(idem: &str) -> MonitorSpec {
        MonitorSpec::from_yaml(&format!(
            r#"
schema_version: 1
name: t
source: {{ type: mur_run, reference: run-1 }}
idempotency_key: {idem}
created_by: {{ actor: user:test }}
"#
        ))
        .unwrap()
    }

    pub(crate) fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
    }

    #[test]
    fn open_twice_and_migrate_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        MonitorStore::open(d.path()).unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let v: i64 = s
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_USER_VERSION);
        assert!(d.path().join("monitor").join(DB_FILE).exists());
    }

    #[test]
    fn create_is_idempotent_on_active_key() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let a = s.create(&spec("k1"), t0(), None).unwrap();
        let b = s.create(&spec("k1"), t0(), None).unwrap();
        assert!(!a.existing);
        assert!(b.existing);
        assert_eq!(a.id, b.id);
        assert_eq!(a.next_check_at, t0(), "first check is immediate");
        let row = s.get(&a.id).unwrap().unwrap();
        assert_eq!(row.state, MonitorState::Active);
        assert_eq!(row.outcome, Outcome::Pending);
        assert_eq!(row.work_started_at, t0());
        assert_eq!(row.fence, 0);
        // a completed monitor no longer reserves its key
        assert!(s.set_state(&a.id, MonitorState::Completed, t0()).unwrap());
        let c = s.create(&spec("k1"), t0(), None).unwrap();
        assert!(!c.existing);
        assert_ne!(c.id, a.id);
    }

    #[test]
    fn work_started_at_is_the_callers_when_given() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let started = t0() - chrono::Duration::minutes(30);
        let c = s.create(&spec("k2"), t0(), Some(started)).unwrap();
        assert_eq!(s.get(&c.id).unwrap().unwrap().work_started_at, started);
    }

    #[test]
    fn list_hides_completed_by_default_and_filters_by_state() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let a = s.create(&spec("a"), t0(), None).unwrap();
        let b = s.create(&spec("b"), t0(), None).unwrap();
        s.set_state(&a.id, MonitorState::Completed, t0()).unwrap();
        s.set_state(&b.id, MonitorState::Exhausted, t0()).unwrap();
        let ids = |f: &ListFilter| -> Vec<String> {
            s.list(f).unwrap().into_iter().map(|r| r.id).collect()
        };
        assert_eq!(
            ids(&ListFilter::default()),
            vec![b.id.clone()],
            "exhausted needs a human, completed does not"
        );
        assert_eq!(
            ids(&ListFilter {
                include_completed: true,
                ..Default::default()
            })
            .len(),
            2
        );
        assert_eq!(
            ids(&ListFilter {
                state: Some(MonitorState::Completed),
                include_completed: true
            }),
            vec![a.id]
        );
    }

    #[test]
    fn reactivate_only_from_exhausted() {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let a = s.create(&spec("a"), t0(), None).unwrap();
        assert!(
            !s.reactivate(&a.id, t0(), false).unwrap(),
            "active is not retryable"
        );
        s.set_state(&a.id, MonitorState::Exhausted, t0()).unwrap();
        s.conn()
            .execute(
                "UPDATE monitors SET remediation_attempts = 3, unknown_streak = 9 WHERE id = ?1",
                [&a.id],
            )
            .unwrap();
        assert!(s.reactivate(&a.id, t0(), false).unwrap());
        let r = s.get(&a.id).unwrap().unwrap();
        assert_eq!(r.state, MonitorState::Active);
        assert_eq!(r.unknown_streak, 0);
        assert_eq!(r.remediation_attempts, 3, "budget kept unless asked");
        s.set_state(&a.id, MonitorState::Exhausted, t0()).unwrap();
        assert!(s.reactivate(&a.id, t0(), true).unwrap());
        assert_eq!(s.get(&a.id).unwrap().unwrap().remediation_attempts, 0);
    }

    /// Rule 6 under the deployment this file's module doc describes: the
    /// CLI, the daemon and (plan-2) an agent runtime each open independent
    /// connections to the same `monitors.db`. A single in-process
    /// connection (every other test here) can never observe a cross-process
    /// race, so this test opens one `MonitorStore` per thread against the
    /// same on-disk file and lines them up with a `Barrier` to force
    /// concurrent `create()` calls on the same idempotency key.
    #[test]
    fn concurrent_create_on_one_key_never_errors_and_never_duplicates() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        // Migrate once up front so every thread's own `open()` below only
        // has to race on `create()`, not on schema creation too.
        MonitorStore::open(&dir).unwrap();

        const N: usize = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
        let handles: Vec<_> = (0..N)
            .map(|_| {
                let dir = dir.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let s = MonitorStore::open(&dir).unwrap();
                    barrier.wait();
                    s.create(&spec("race"), t0(), None)
                })
            })
            .collect();
        let results: Vec<Result<Created>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        let mut ids = std::collections::HashSet::new();
        let (mut created, mut existing) = (0, 0);
        for r in &results {
            let c = r
                .as_ref()
                .expect("create() must never error under a racing insert");
            ids.insert(c.id.clone());
            if c.existing {
                existing += 1;
            } else {
                created += 1;
            }
        }
        assert_eq!(created, 1, "exactly one caller creates the monitor");
        assert_eq!(existing, N - 1, "everyone else finds it existing");
        assert_eq!(ids.len(), 1, "every caller must agree on the same id");

        let s = MonitorStore::open(&dir).unwrap();
        assert_eq!(
            s.list(&ListFilter {
                include_completed: true,
                ..Default::default()
            })
            .unwrap()
            .len(),
            1,
            "no duplicate row was inserted"
        );
    }
}
