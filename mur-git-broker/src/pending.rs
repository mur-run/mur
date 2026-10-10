//! Persistent pending-request table (design §4).
//!
//! A request is identified by `(agent_id, task_id, request_id)`. Rows move only along
//! `ALLOWED_EDGES`, by compare-and-swap, and only on explicit events. Nothing here ages a row
//! out: a request waiting for a human stays `PendingApproval` until someone acts.

use crate::{
    action::ActionDocument,
    constants::{PENDING_DB_BUSY_WAIT_MS, PENDING_DB_MODE},
    error::BrokerError,
    policy::BrokerLimits,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Validated,
    PendingApproval,
    Approved,
    Executing,
    Succeeded,
    Denied,
    Cancelled,
    ApprovalExpired,
    PolicyChanged,
    StaleOldSha,
    Rejected,
    Failed,
    OutcomeUnknown,
}

impl State {
    const ALL: [State; 13] = [
        State::Validated,
        State::PendingApproval,
        State::Approved,
        State::Executing,
        State::Succeeded,
        State::Denied,
        State::Cancelled,
        State::ApprovalExpired,
        State::PolicyChanged,
        State::StaleOldSha,
        State::Rejected,
        State::Failed,
        State::OutcomeUnknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Validated => "validated",
            State::PendingApproval => "pending_approval",
            State::Approved => "approved",
            State::Executing => "executing",
            State::Succeeded => "succeeded",
            State::Denied => "denied",
            State::Cancelled => "cancelled",
            State::ApprovalExpired => "approval_expired",
            State::PolicyChanged => "policy_changed",
            State::StaleOldSha => "stale_old_sha",
            State::Rejected => "rejected",
            State::Failed => "failed",
            State::OutcomeUnknown => "outcome_unknown",
        }
    }

    fn parse(s: &str) -> Result<State, BrokerError> {
        State::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| BrokerError::Storage(format!("unknown state in table: {s}")))
    }
}

/// Design §4 diagram, plus the plan's `validated → pending_approval`, `approved → executing`
/// and the terminal edges out of `executing`. Policy invalidation also ends a pending row.
pub const ALLOWED_EDGES: &[(State, State)] = &[
    (State::Validated, State::PendingApproval),
    (State::PendingApproval, State::Approved),
    (State::PendingApproval, State::Denied),
    (State::PendingApproval, State::Cancelled),
    (State::PendingApproval, State::PolicyChanged),
    (State::Approved, State::Executing),
    (State::Approved, State::ApprovalExpired),
    (State::Approved, State::PolicyChanged),
    (State::Approved, State::StaleOldSha),
    (State::Executing, State::Succeeded),
    (State::Executing, State::Rejected),
    (State::Executing, State::Failed),
    (State::Executing, State::OutcomeUnknown),
    (State::Executing, State::StaleOldSha),
];

/// States that count against `max_pending_per_agent`.
const LIVE_STATES: &str = "'validated','pending_approval','approved','executing'";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RequestKey {
    pub agent_id: String,
    pub task_id: String,
    pub request_id: String,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub key: RequestKey,
    pub state: State,
    pub action_hash: String,
    pub created_at: DateTime<Utc>,
    /// Set by the approval step (T10); always `None` for a row this module created.
    pub accepted_at: Option<DateTime<Utc>>,
    pub doc_json: String,
}

#[derive(Debug)]
pub enum Submitted {
    New,
    Existing(Row),
}

pub struct PendingStore {
    conn: Mutex<Connection>,
}

fn storage(e: impl std::fmt::Display) -> BrokerError {
    BrokerError::Storage(e.to_string())
}

fn from_millis(ms: i64) -> Result<DateTime<Utc>, BrokerError> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .ok_or_else(|| BrokerError::Storage(format!("timestamp out of range: {ms}")))
}

const SELECT_ROW: &str = "SELECT agent_id, task_id, request_id, state, action_hash, \
                          created_at, accepted_at, doc_json FROM requests";

fn read_row(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<(RequestKey, String, String, i64, Option<i64>, String)> {
    Ok((
        RequestKey {
            agent_id: r.get(0)?,
            task_id: r.get(1)?,
            request_id: r.get(2)?,
        },
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
    ))
}

fn build_row(
    raw: (RequestKey, String, String, i64, Option<i64>, String),
) -> Result<Row, BrokerError> {
    let (key, state, action_hash, created, accepted, doc_json) = raw;
    Ok(Row {
        key,
        state: State::parse(&state)?,
        action_hash,
        created_at: from_millis(created)?,
        accepted_at: accepted.map(from_millis).transpose()?,
        doc_json,
    })
}

impl PendingStore {
    pub fn open(path: &Path) -> Result<Self, BrokerError> {
        let conn = Connection::open(path).map_err(storage)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(PENDING_DB_MODE))
            .map_err(storage)?;
        conn.busy_timeout(std::time::Duration::from_millis(PENDING_DB_BUSY_WAIT_MS))
            .map_err(storage)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS requests (
                 agent_id    TEXT NOT NULL,
                 task_id     TEXT NOT NULL,
                 request_id  TEXT NOT NULL,
                 state       TEXT NOT NULL,
                 action_hash TEXT NOT NULL,
                 created_at  INTEGER NOT NULL,
                 accepted_at INTEGER,
                 doc_json    TEXT NOT NULL,
                 PRIMARY KEY (agent_id, task_id, request_id)
             );
             CREATE INDEX IF NOT EXISTS requests_by_agent ON requests (agent_id, created_at);
             CREATE TABLE IF NOT EXISTS tombstones (
                 event_id   TEXT PRIMARY KEY,
                 request_id TEXT NOT NULL,
                 ts         INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS frozen (
                 agent_id       TEXT NOT NULL,
                 task_id        TEXT NOT NULL,
                 request_id     TEXT NOT NULL,
                 control_digest TEXT NOT NULL,
                 PRIMARY KEY (agent_id, task_id, request_id)
             );
             CREATE TABLE IF NOT EXISTS audit (
                 id       INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind     TEXT NOT NULL,
                 agent_id TEXT NOT NULL,
                 ts       INTEGER NOT NULL
             );",
        )
        .map_err(storage)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, BrokerError> {
        self.conn
            .lock()
            .map_err(|_| BrokerError::Storage("pending store lock poisoned".into()))
    }

    /// Insert a new `Validated` row, or return the existing one for an identical resubmission.
    /// One `IMMEDIATE` transaction: the cap checks and the insert cannot interleave with another
    /// submitter. A rejection writes an `audit` row and nothing else.
    pub fn submit(
        &self,
        key: &RequestKey,
        doc: &ActionDocument,
        hash: &str,
        now: DateTime<Utc>,
        limits: &BrokerLimits,
    ) -> Result<Submitted, BrokerError> {
        if doc.agent_id != key.agent_id
            || doc.task_id != key.task_id
            || doc.request_id != key.request_id
        {
            return Err(BrokerError::InvalidRequest(
                "key does not match the action document".into(),
            ));
        }
        if doc.action_hash()? != hash {
            return Err(BrokerError::InvalidRequest(
                "action_hash does not match the document".into(),
            ));
        }
        let doc_json = serde_json::to_string(doc).map_err(storage)?;
        let mut guard = self.lock()?;
        let tx = guard
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;

        let existing = tx
            .query_row(
                &format!("{SELECT_ROW} WHERE agent_id=?1 AND task_id=?2 AND request_id=?3"),
                params![key.agent_id, key.task_id, key.request_id],
                read_row,
            )
            .optional()
            .map_err(storage)?
            .map(build_row)
            .transpose()?;

        let rejection = match existing {
            Some(row) if row.action_hash == hash => return Ok(Submitted::Existing(row)),
            Some(_) => Some(BrokerError::RequestConflict),
            None => {
                let live: i64 = tx
                    .query_row(
                        &format!("SELECT COUNT(*) FROM requests WHERE agent_id=?1 AND state IN ({LIVE_STATES})"),
                        params![key.agent_id],
                        |r| r.get(0),
                    )
                    .map_err(storage)?;
                let since =
                    (now - Duration::seconds(limits.request_window_secs)).timestamp_millis();
                let recent: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM requests WHERE agent_id=?1 AND created_at>?2",
                        params![key.agent_id, since],
                        |r| r.get(0),
                    )
                    .map_err(storage)?;
                if live as usize >= limits.max_pending_per_agent {
                    Some(BrokerError::QueueFull)
                } else if recent as usize >= limits.max_new_requests_per_window {
                    Some(BrokerError::RateLimited)
                } else {
                    None
                }
            }
        };

        if let Some(err) = rejection {
            tx.execute(
                "INSERT INTO audit (kind, agent_id, ts) VALUES (?1, ?2, ?3)",
                params![err.code(), key.agent_id, now.timestamp_millis()],
            )
            .map_err(storage)?;
            tx.commit().map_err(storage)?;
            return Err(err);
        }

        tx.execute(
            "INSERT INTO requests (agent_id, task_id, request_id, state, action_hash, created_at, doc_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                key.agent_id,
                key.task_id,
                key.request_id,
                State::Validated.as_str(),
                hash,
                now.timestamp_millis(),
                doc_json
            ],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(Submitted::New)
    }

    /// Compare-and-swap `from → to`. An edge outside `ALLOWED_EDGES`, or a row that is no longer
    /// in `from`, is `NotPending` and leaves the row untouched.
    pub fn transition(&self, key: &RequestKey, from: State, to: State) -> Result<(), BrokerError> {
        if !ALLOWED_EDGES.contains(&(from, to)) {
            return Err(BrokerError::NotPending);
        }
        let changed = self
            .lock()?
            .execute(
                "UPDATE requests SET state=?1 WHERE agent_id=?2 AND task_id=?3 AND request_id=?4 AND state=?5",
                params![to.as_str(), key.agent_id, key.task_id, key.request_id, from.as_str()],
            )
            .map_err(storage)?;
        if changed == 0 {
            return Err(BrokerError::NotPending);
        }
        Ok(())
    }

    /// Atomically consume one approval event: the row must be `pending_approval` with exactly
    /// this `request_id` and `action_hash`, and the event id must never have been used. On success
    /// the row becomes `approved`, `accepted_at = now`, and the event is tombstoned. Any other
    /// outcome changes nothing.
    pub fn accept(
        &self,
        key: &RequestKey,
        event_id: &str,
        request_id: &str,
        action_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<(), BrokerError> {
        let mut guard = self.lock()?;
        let tx = guard
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let used: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM tombstones WHERE event_id=?1",
                params![event_id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if used > 0 || request_id != key.request_id {
            return Err(BrokerError::NotPending);
        }
        let changed = tx
            .execute(
                "UPDATE requests SET state=?1, accepted_at=?2 \
                 WHERE agent_id=?3 AND task_id=?4 AND request_id=?5 AND state=?6 AND action_hash=?7",
                params![
                    State::Approved.as_str(),
                    now.timestamp_millis(),
                    key.agent_id,
                    key.task_id,
                    key.request_id,
                    State::PendingApproval.as_str(),
                    action_hash
                ],
            )
            .map_err(storage)?;
        if changed == 0 {
            return Err(BrokerError::NotPending);
        }
        tx.execute(
            "INSERT INTO tombstones (event_id, request_id, ts) VALUES (?1, ?2, ?3)",
            params![event_id, request_id, now.timestamp_millis()],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }

    /// `validated → pending_approval`, recording the control digest the private repo was frozen
    /// at, in one transaction.
    pub fn to_pending(&self, key: &RequestKey, control_digest: &str) -> Result<(), BrokerError> {
        let mut guard = self.lock()?;
        let tx = guard
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let changed = tx
            .execute(
                "UPDATE requests SET state=?1 WHERE agent_id=?2 AND task_id=?3 AND request_id=?4 AND state=?5",
                params![
                    State::PendingApproval.as_str(),
                    key.agent_id,
                    key.task_id,
                    key.request_id,
                    State::Validated.as_str()
                ],
            )
            .map_err(storage)?;
        if changed == 0 {
            return Err(BrokerError::NotPending);
        }
        tx.execute(
            "INSERT OR REPLACE INTO frozen (agent_id, task_id, request_id, control_digest) VALUES (?1, ?2, ?3, ?4)",
            params![key.agent_id, key.task_id, key.request_id, control_digest],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }

    pub fn frozen_digest(&self, key: &RequestKey) -> Result<Option<String>, BrokerError> {
        self.lock()?
            .query_row(
                "SELECT control_digest FROM frozen WHERE agent_id=?1 AND task_id=?2 AND request_id=?3",
                params![key.agent_id, key.task_id, key.request_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)
    }

    /// Drop a row that never got past `validated` (its git work failed). Any other state is
    /// left alone.
    pub fn discard_validated(&self, key: &RequestKey) -> Result<(), BrokerError> {
        self.lock()?
            .execute(
                "DELETE FROM requests WHERE agent_id=?1 AND task_id=?2 AND request_id=?3 AND state='validated'",
                params![key.agent_id, key.task_id, key.request_id],
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Startup recovery. A `validated` row has no durable git state worth keeping, so it goes. A
    /// row still `executing` means the process died around the push: nobody knows whether the ref
    /// moved, so it is `outcome_unknown` (the approval stays consumed, never retried).
    pub fn recover(&self) -> Result<(), BrokerError> {
        let guard = self.lock()?;
        guard
            .execute("DELETE FROM requests WHERE state='validated'", [])
            .map_err(storage)?;
        guard
            .execute(
                "UPDATE requests SET state=?1 WHERE state=?2",
                params![State::OutcomeUnknown.as_str(), State::Executing.as_str()],
            )
            .map_err(storage)?;
        Ok(())
    }

    /// Record a refused or failed request in the audit table.
    pub fn audit(&self, agent: &str, kind: &str, now: DateTime<Utc>) -> Result<(), BrokerError> {
        self.lock()?
            .execute(
                "INSERT INTO audit (kind, agent_id, ts) VALUES (?1, ?2, ?3)",
                params![kind, agent, now.timestamp_millis()],
            )
            .map_err(storage)?;
        Ok(())
    }

    pub fn get(&self, key: &RequestKey) -> Result<Option<Row>, BrokerError> {
        self.lock()?
            .query_row(
                &format!("{SELECT_ROW} WHERE agent_id=?1 AND task_id=?2 AND request_id=?3"),
                params![key.agent_id, key.task_id, key.request_id],
                read_row,
            )
            .optional()
            .map_err(storage)?
            .map(build_row)
            .transpose()
    }

    /// Rows waiting for a human, oldest first.
    pub fn list_pending(&self) -> Result<Vec<Row>, BrokerError> {
        let guard = self.lock()?;
        let mut stmt = guard
            .prepare(&format!(
                "{SELECT_ROW} WHERE state=?1 ORDER BY created_at, rowid"
            ))
            .map_err(storage)?;
        let raws = stmt
            .query_map(params![State::PendingApproval.as_str()], read_row)
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        raws.into_iter().map(build_row).collect()
    }

    pub fn audit_count(&self, agent: &str, kind: &str) -> Result<u64, BrokerError> {
        let n: i64 = self
            .lock()?
            .query_row(
                "SELECT COUNT(*) FROM audit WHERE agent_id=?1 AND kind=?2",
                params![agent, kind],
                |r| r.get(0),
            )
            .map_err(storage)?;
        Ok(n as u64)
    }

    /// Test seam: set a state without going through `ALLOWED_EDGES`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn force_state_for_test(&self, key: &RequestKey, s: State) -> Result<(), BrokerError> {
        self.lock()?
            .execute(
                "UPDATE requests SET state=?1 WHERE agent_id=?2 AND task_id=?3 AND request_id=?4",
                params![s.as_str(), key.agent_id, key.task_id, key.request_id],
            )
            .map_err(storage)?;
        Ok(())
    }
}
