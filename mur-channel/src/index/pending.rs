//! The post-migration rebuild as a durable obligation.
//!
//! `migrate()` cannot rebuild by itself (it has no `ChannelStore`), so it
//! records that a rebuild is owed and `ChannelService::open` pays it. The
//! debt lives in a row, not in "did this open's `ALTER TABLE` succeed": that
//! signal fires once per DB file, so a rebuild that failed on its one chance
//! was never retried, and a DB holding folded rows without fold-ledger
//! entries double-counted every rerun of those seqs. The row is set in the
//! same transaction as the schema change that needs it and cleared in the
//! same transaction as the rebuild that pays it, so neither a crash nor a
//! failure can separate the two.

use anyhow::{Context, Result};

use super::ChannelIndex;
use crate::store::ChannelStore;

pub(super) const META_DDL: &str = "CREATE TABLE IF NOT EXISTS index_meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) WITHOUT ROWID;";

const REBUILD_PENDING: &str = "rebuild_pending";

impl ChannelIndex {
    /// Whether a rebuild is owed and has not yet succeeded.
    pub fn rebuild_pending(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM index_meta WHERE key = ?1)",
            [REBUILD_PENDING],
            |r| r.get(0),
        )?)
    }

    /// Pay an owed rebuild, if any. `None` = nothing was owed (including
    /// "another connection paid it first"); `Some(n)` = rebuilt `n` channels.
    ///
    /// The CLI, the Hub and `parallel_jobs` open one DB concurrently and can
    /// all see the flag before any of them rebuilds, so the flag is read
    /// again under the write lock: the losers find it cleared and skip.
    pub fn rebuild_if_pending(&self, store: &ChannelStore) -> Result<Option<usize>> {
        if !self.rebuild_pending()? {
            return Ok(None);
        }
        self.write_txn("rebuild", || {
            if self.rebuild_pending()? {
                self.rebuild_locked(store).map(Some)
            } else {
                Ok(None)
            }
        })
    }

    pub(super) fn set_rebuild_pending(&self) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO index_meta (key, value) VALUES (?1, '1')",
            [REBUILD_PENDING],
        )?;
        Ok(())
    }

    pub(super) fn clear_rebuild_pending(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM index_meta WHERE key = ?1", [REBUILD_PENDING])?;
        Ok(())
    }

    /// A channel that has folded events (`last_seq >= 0`) but no fold-ledger
    /// rows. Every writer keeps the two in step (the fold writes both in one
    /// savepoint; `remove` and the rebuild clear both), so this only holds
    /// on a DB whose ledger was never seeded — e.g. the ledger migration ran
    /// but its rebuild failed under a build that did not retry. Derived from
    /// the data rather than a marker, so such a DB heals on its next open.
    pub(super) fn ledger_missing(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM channels c WHERE c.last_seq >= 0
               AND NOT EXISTS (SELECT 1 FROM channel_folds f WHERE f.channel_id = c.id))",
            [],
            |r| r.get(0),
        )?)
    }

    /// Run `f` inside `BEGIN IMMEDIATE` / `COMMIT`, rolling back on any
    /// error. `&self` rules out rusqlite's `transaction()` (it needs
    /// `&mut Connection`), so the transaction is driven by hand.
    /// `IMMEDIATE` takes the write lock up front, so a lock conflict
    /// surfaces before anything is touched rather than halfway through.
    pub(super) fn write_txn<T>(&self, what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .with_context(|| format!("begin {what} transaction"))?;
        match f() {
            Ok(v) => match self.conn.execute_batch("COMMIT") {
                Ok(()) => Ok(v),
                Err(e) => {
                    // COMMIT itself failed (e.g. disk full). Without the
                    // rollback the transaction stays open and every later
                    // write on this connection silently queues inside it.
                    // A failing rollback must not mask the commit error.
                    let _ = self.conn.execute_batch("ROLLBACK");
                    Err(e).with_context(|| format!("commit {what} transaction"))
                }
            },
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }
}
