use std::path::Path;

use anyhow::{Context, Result};
use mur_common::channel::{Channel, ChannelActor, ChannelEvent, EventKind};
use rusqlite::{Connection, OptionalExtension};

use crate::store::ChannelStore;

/// Side table holding the stable display number per channel id. Deliberately
/// *not* a column on `channels`: `rebuild_from` clears that table, and an
/// ordinal must outlive both a rebuild and the channel's own deletion (numbers
/// are never recycled, so a user's remembered "#2" never points somewhere new).
const ORDINAL_DDL: &str = "CREATE TABLE IF NOT EXISTS channel_ordinals (
        id      TEXT PRIMARY KEY,
        ordinal INTEGER NOT NULL UNIQUE
    );";

/// Which seqs of each channel have been folded into `channels`. This is the
/// fold dedup: membership, not `seq > last_seq`, so two processes folding
/// one channel out of order both land. `kind` lets the preview and the HITL
/// flag follow the *newest* event of their kind rather than the last one to
/// fold. Derived like the rest of the read model — `rebuild_from` and
/// `remove` clear it alongside `channels`.
const FOLDS_DDL: &str = "CREATE TABLE IF NOT EXISTS channel_folds (
        channel_id TEXT NOT NULL,
        seq        INTEGER NOT NULL,
        kind       TEXT,
        PRIMARY KEY (channel_id, seq)
    ) WITHOUT ROWID;";

/// Additive `channels` columns, applied in order by `migrate()`.
const MIGRATION_DDL: &[&str] = &[
    "ALTER TABLE channels ADD COLUMN purpose TEXT NOT NULL DEFAULT 'conversation'",
    "ALTER TABLE channels ADD COLUMN agents TEXT NOT NULL DEFAULT '[]'",
    "ALTER TABLE channels ADD COLUMN preview TEXT NOT NULL DEFAULT ''",
    "ALTER TABLE channels ADD COLUMN msg_count INTEGER NOT NULL DEFAULT 0",
    // -1, not 0: seqs are 0-indexed, so 0 is a real, foldable seq.
    // `record_event` once deduped on `ev.seq > last_seq`, where a 0
    // default rejected the channel's very first event forever. Dedup
    // now lives in `channel_folds`, but -1 still means "nothing
    // folded yet" without colliding with any real seq.
    "ALTER TABLE channels ADD COLUMN last_seq INTEGER NOT NULL DEFAULT -1",
    "ALTER TABLE channels ADD COLUMN last_read_seq INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE channels ADD COLUMN hitl_pending INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE channels ADD COLUMN inbound_seqs TEXT NOT NULL DEFAULT '[]'",
    // Marker-only column: SQLite cannot ALTER an existing column's
    // DEFAULT, so a DB that already had `last_seq` (added under the old
    // DEFAULT 0, before the -1 sentinel) keeps reporting 0 as its schema
    // default forever. On such a DB this column does not exist yet, so its
    // ADD COLUMN succeeds and owes a rebuild; the rebuild re-inserts every
    // row via `upsert()` (which writes `last_seq = -1` explicitly) and
    // re-derives it from the event log. Never read.
    "ALTER TABLE channels ADD COLUMN seq_sentinel_fix_applied INTEGER NOT NULL DEFAULT 1",
    // Marker-only, same mechanism: a DB that predates the `channel_folds`
    // ledger has rows folded without ledger entries, so a rerun of one of
    // those seqs would double-count. The rebuild this owes seeds the ledger
    // and replays every event, recovering any message the old
    // `seq > last_seq` guard dropped. (`ledger_missing` re-detects the same
    // state from the data, for a DB that got this column but not the
    // rebuild.) Never read.
    "ALTER TABLE channels ADD COLUMN fold_ledger_applied INTEGER NOT NULL DEFAULT 1",
];

/// SQLite read-model at `<mur_home>/index/channels/channels.db`. Droppable &
/// rebuildable from the event-log manifests — never the source of truth.
pub struct ChannelIndex {
    conn: Connection,
}

/// One row of the channel-list / "my work" query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRow {
    pub id: String,
    pub title: String,
    pub state: String,
    pub updated_at: String,
    /// `effective_purpose` resolved at write time, kebab-case.
    pub purpose: String,
    /// JSON array of agent participant ids, e.g. `["mur"]`.
    pub agents: String,
    /// Text of the most recent human-visible message.
    pub preview: String,
    /// Human-visible message count (drives unread, never turn totals).
    pub msg_count: i64,
    /// Highest event seq seen.
    pub last_seq: i64,
    /// Read watermark (Task 7).
    pub last_read_seq: i64,
    /// A HITL request is awaiting a response.
    pub hitl_pending: bool,
    /// JSON array of the seqs of messages not authored by the local human.
    /// Small (bounded by message count), trivially rebuildable from the
    /// event log; makes unread a filter rather than a subtraction.
    pub inbound_seqs: String,
    /// Stable per-machine display number, assigned in creation order on the
    /// first upsert of an id and never reused — deleting a channel burns its
    /// number. It is the handle humans type (`/channels 2`) and the label the
    /// Hub shows, so it must not shift when the list reorders.
    ///
    /// **Local view state only.** Ordinals are never part of a signed channel
    /// event and are never synced: the same channel may be #2 here and #7 on
    /// another machine. Do not add it to any wire type.
    pub ordinal: i64,
}

impl ChannelIndex {
    pub fn open(mur_home: &Path) -> Result<Self> {
        let dir = mur_home.join("index").join("channels");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        // One-time best-effort migration from the old `<mur_home>/index/`
        // location (pre-narrowing) to the new `<mur_home>/index/channels/`
        // subdir. This is deliberately best-effort: if a sandboxed process
        // (which can write the new subdir but not the parent `index/` dir)
        // races this rename, the rename fails silently and the index simply
        // starts fresh at the new path — it's a droppable/rebuildable
        // read-model that self-heals per-channel on the next write, so
        // losing the race here is harmless.
        let old_dir = mur_home.join("index");
        for ext in ["", "-wal", "-shm"] {
            let old = old_dir.join(format!("channels.db{ext}"));
            let new = dir.join(format!("channels.db{ext}"));
            if old.exists() && !new.exists() {
                let _ = std::fs::rename(&old, &new);
            }
        }

        let conn = Connection::open(dir.join("channels.db")).context("open channels.db")?;
        // Concurrency: the CLI and Hub open independent connections to this DB.
        // WAL allows concurrent readers alongside one writer; busy_timeout makes a
        // contended writer wait briefly instead of failing immediately with
        // SQLITE_BUSY (which would drop a turn under normal Hub+CLI use).
        //
        // Order matters: set busy_timeout BEFORE journal_mode=WAL. Switching the
        // journal mode takes a lock on the database, and when several processes
        // open the DB concurrently (e.g. parallel_jobs fanning out) that lock is
        // contended — without a busy_timeout already in effect the WAL switch
        // itself fails immediately with SQLITE_BUSY. Setting the timeout first
        // makes the contended connection wait and retry instead of dropping.
        conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL;")
            .context("configure channels.db concurrency pragmas")?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    /// Bring the schema up to date and record whether a rebuild is owed
    /// (see `pending.rs`). `ChannelService::open` pays the debt; `migrate`
    /// cannot, since it has no `ChannelStore`.
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS channels (
                id            TEXT PRIMARY KEY,
                title         TEXT NOT NULL,
                state         TEXT NOT NULL,
                owner         TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_channels_updated ON channels(updated_at DESC);",
        )?;
        self.conn.execute_batch(pending::META_DDL)?;
        self.conn.execute_batch(FOLDS_DDL)?;
        // Additive columns. `ADD COLUMN` on an existing column errors; that is
        // the "already migrated" case for that column, and its success is
        // the "this DB predates the column" signal that owes a rebuild.
        //
        // One transaction for the columns and the flag they imply, so a
        // crash between a successful ALTER (which never succeeds again) and
        // the flag write cannot lose the rebuild. A failed ALTER rolls back
        // only its own statement, not the transaction.
        self.write_txn("schema migration", || {
            let mut migrated = false;
            for ddl in MIGRATION_DDL {
                if self.conn.execute(ddl, []).is_ok() {
                    migrated = true;
                }
            }
            if migrated || self.ledger_missing()? {
                self.set_rebuild_pending()?;
            }
            Ok(())
        })?;
        // Stable display numbers live in their own table, not a column on
        // `channels`, for two reasons: `rebuild_from` wipes `channels` and
        // would otherwise renumber everything, and a number must stay burned
        // after its channel is deleted. Nothing here is ever deleted.
        self.conn.execute_batch(ORDINAL_DDL)?;
        // Backfill for DBs that predate the table. Idempotent: `OR IGNORE`
        // plus the PRIMARY KEY means already-numbered ids are skipped, and
        // the ordering gives existing channels the numbers a user expects.
        //
        // Inside `BEGIN IMMEDIATE` because the CLI and the Hub are expected
        // to open a cold DB concurrently. `(SELECT MAX(ordinal))` reads the
        // base once per statement, so two interleaved backfills can both
        // compute the same base; `OR IGNORE` then makes the loser's rows
        // vanish rather than collide, leaving channels unnumbered until some
        // later open happens to retry. Taking the write lock up front
        // serialises the read-then-insert so the second run sees the first
        // run's numbers.
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .context("begin ordinal backfill")?;
        let backfill = self.conn.execute(
            "INSERT OR IGNORE INTO channel_ordinals (id, ordinal)
             SELECT id, (SELECT IFNULL(MAX(ordinal), 0) FROM channel_ordinals)
                         + ROW_NUMBER() OVER (ORDER BY created_at ASC, rowid ASC)
             FROM channels WHERE id NOT IN (SELECT id FROM channel_ordinals)",
            [],
        );
        match backfill {
            Ok(_) => self
                .conn
                .execute_batch("COMMIT")
                .context("commit ordinal backfill")?,
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(e).context("backfill channel ordinals");
            }
        }
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_channels_purpose ON channels(purpose, updated_at DESC);",
        )?;
        // Rebuildable full-text projection of message bodies. This is a
        // standalone (not external-content) FTS5 table, so it stores its own
        // copy of `body` — that copy is what `snippet()` reads from below.
        self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS channel_fts
             USING fts5(channel_id UNINDEXED, seq UNINDEXED, body, tokenize='unicode61');",
        )?;
        Ok(())
    }

    pub fn upsert(&self, ch: &Channel) -> Result<()> {
        let owner = serde_json::to_string(&ch.owner)?;
        let purpose = serde_json::to_string(&crate::purpose::effective_purpose(ch))?
            .trim_matches('"')
            .to_string();
        let agents: Vec<&str> = ch
            .participants
            .iter()
            .filter_map(|p| match &p.actor {
                mur_common::channel::ChannelActor::Agent { id } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let agents = serde_json::to_string(&agents)?;
        // `last_seq` is written explicitly as -1 (not left to the column
        // DEFAULT) so a new row is correctly seeded as "nothing folded yet"
        // even on a DB where the column's actual DEFAULT is stuck at the old
        // value 0 (SQLite cannot ALTER an existing column's default — see
        // the migration comment above). ON CONFLICT deliberately does not
        // touch last_seq: it must never reset an existing row's fold
        // progress back to -1.
        self.conn.execute(
            "INSERT INTO channels (id,title,state,owner,created_at,updated_at,purpose,agents,last_seq)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,-1)
             ON CONFLICT(id) DO UPDATE SET
               title=excluded.title, state=excluded.state,
               owner=excluded.owner, updated_at=excluded.updated_at,
               purpose=excluded.purpose, agents=excluded.agents",
            rusqlite::params![
                ch.id,
                ch.title,
                serde_json::to_string(&ch.state)?.trim_matches('"'),
                owner,
                ch.created_at.to_rfc3339(),
                ch.updated_at.to_rfc3339(),
                purpose,
                agents,
            ],
        )?;
        // First sighting of this id gets the next number; `OR IGNORE` makes
        // every later upsert (and every rebuild) a no-op, which is what keeps
        // the number stable.
        self.conn.execute(
            "INSERT OR IGNORE INTO channel_ordinals (id, ordinal)
             VALUES (?1, (SELECT IFNULL(MAX(ordinal), 0) + 1 FROM channel_ordinals))",
            [&ch.id],
        )?;
        Ok(())
    }

    /// Fold one freshly-appended event into the read model.
    ///
    /// Incremental on purpose: rescanning every event on every append is O(n²)
    /// over a conversation's life. `rebuild_from` is the slow, authoritative
    /// path when the index is thrown away.
    pub fn record_event(&self, ch_id: &str, ev: &ChannelEvent) -> Result<()> {
        let counts = matches!(ev.kind, EventKind::Message);
        let preview = if counts {
            ev.payload
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        } else {
            ""
        };
        let fold_kind = match ev.kind {
            EventKind::Message => Some("msg"),
            EventKind::HitlRequest | EventKind::HitlResponse => Some("hitl"),
            _ => None,
        };
        let hitl_pending = match ev.kind {
            EventKind::HitlRequest => Some(1_i64),
            EventKind::HitlResponse => Some(0_i64),
            _ => None,
        };
        let inbound = counts && !matches!(ev.actor, ChannelActor::Human { .. });
        let seq = ev.seq as i64;
        // Dedup on `(channel_id, seq)` membership in `channel_folds`, not on
        // `seq > last_seq`. A crash-rerun that hits `ChannelStore::append_event`'s
        // idempotency-key dedup returns the *same* event, and folding it again
        // must be a no-op (else msg_count, inbound_seqs and the FTS row all
        // duplicate). Membership gives that without assuming order: the
        // store releases `events.lock` before this fold runs and the
        // `channel/delegate` path has a second process writing the same
        // channel, so seqs can arrive out of order — and a lower, late seq is
        // still a real message that must be counted and searchable.
        //
        // A SAVEPOINT (not BEGIN) so this nests inside `rebuild_from`'s
        // transaction; the ledger insert is the first statement, so the
        // write lock is taken before anything is read.
        self.conn.execute_batch("SAVEPOINT fold")?;
        let result = (|| -> Result<()> {
            let fresh = self.conn.execute(
                "INSERT OR IGNORE INTO channel_folds (channel_id, seq, kind)
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM channels WHERE id = ?1)",
                rusqlite::params![ch_id, seq, fold_kind],
            )?;
            if fresh == 0 {
                return Ok(());
            }
            // `newest` = no other folded event of this kind has a higher seq,
            // so a late-arriving older message/gate event never overwrites
            // the preview or the HITL flag set by a newer one.
            let newest: bool = match fold_kind {
                Some(kind) => self.conn.query_row(
                    "SELECT NOT EXISTS (SELECT 1 FROM channel_folds
                       WHERE channel_id = ?1 AND kind = ?2 AND seq > ?3)",
                    rusqlite::params![ch_id, kind, seq],
                    |r| r.get(0),
                )?,
                None => false,
            };
            self.conn.execute(
                "UPDATE channels SET
                   last_seq  = MAX(last_seq, ?2),
                   msg_count = msg_count + ?3,
                   preview   = CASE WHEN ?3 = 1 AND ?7 = 1 THEN ?4 ELSE preview END,
                   hitl_pending = CASE WHEN ?7 = 1 THEN COALESCE(?5, hitl_pending)
                       ELSE hitl_pending END,
                   inbound_seqs = CASE WHEN ?6 = 1
                       THEN json_insert(inbound_seqs, '$[#]', ?2)
                       ELSE inbound_seqs END
                 WHERE id = ?1",
                rusqlite::params![
                    ch_id,
                    seq,
                    if counts { 1_i64 } else { 0_i64 },
                    preview,
                    hitl_pending,
                    if inbound { 1_i64 } else { 0_i64 },
                    if newest { 1_i64 } else { 0_i64 },
                ],
            )?;
            if counts && !preview.is_empty() {
                self.conn.execute(
                    "INSERT INTO channel_fts (channel_id, seq, body) VALUES (?1, ?2, ?3)",
                    rusqlite::params![ch_id, seq, preview],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.conn.execute_batch("RELEASE fold")?,
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK TO fold; RELEASE fold");
                return Err(e);
            }
        }
        Ok(())
    }

    /// Full-text matches, newest-activity-first. Returns `(channel_id, seq, snippet)`.
    ///
    /// The query is passed to FTS5 as a single quoted phrase: user input is
    /// typed mid-search and must never be interpreted as FTS operator syntax
    /// (a bare `"` would otherwise be a hard error).
    pub fn search_bodies(&self, query: &str, limit: usize) -> Result<Vec<(String, i64, String)>> {
        let phrase = format!("\"{}\"", query.replace('"', " "));
        if phrase.trim_matches(['"', ' ']).is_empty() {
            return Ok(vec![]);
        }
        let mut stmt = self.conn.prepare(
            "SELECT f.channel_id, f.seq, snippet(channel_fts, 2, '', '', '…', 12)
             FROM channel_fts f
             JOIN channels c ON c.id = f.channel_id
             WHERE channel_fts MATCH ?1
             ORDER BY c.updated_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![phrase, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Raise the read watermark. Monotonic: a stale surface reporting an older
    /// position can never resurrect already-cleared unread state.
    pub fn mark_read(&self, ch_id: &str, seq: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE channels SET last_read_seq = MAX(last_read_seq, ?2) WHERE id = ?1",
            rusqlite::params![ch_id, seq as i64],
        )?;
        Ok(())
    }

    /// The stable number assigned to `ch_id`, or `None` if the channel has no
    /// ordinal yet (not indexed, or an index row awaiting backfill).
    pub fn ordinal_of(&self, ch_id: &str) -> Result<Option<u64>> {
        let n: Option<i64> = self
            .conn
            .query_row(
                "SELECT ordinal FROM channel_ordinals WHERE id = ?1",
                [ch_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match n {
            None => None,
            // NOT NULL UNIQUE, only ever written as MAX+1 — a negative value
            // is corruption. Clamping it would produce 0, which already means
            // "not numbered yet", so the caller could not tell the two apart.
            Some(n) if n < 0 => {
                anyhow::bail!("channel {ch_id} has a corrupt ordinal in the index: {n}")
            }
            Some(n) => Some(n as u64),
        })
    }

    /// Newest-first channel list (the Hub left-rail / CLI "my work" inbox).
    pub fn list(&self, limit: usize) -> Result<Vec<ChannelRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id,c.title,c.state,c.updated_at,c.purpose,c.agents,c.preview,c.msg_count,c.last_seq,c.last_read_seq,c.hitl_pending,c.inbound_seqs,IFNULL(o.ordinal,0)
             FROM channels c LEFT JOIN channel_ordinals o ON o.id = c.id
             ORDER BY c.updated_at DESC, c.rowid DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit as i64], |r| {
                Ok(ChannelRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    state: r.get(2)?,
                    updated_at: r.get(3)?,
                    purpose: r.get(4)?,
                    agents: r.get(5)?,
                    preview: r.get(6)?,
                    msg_count: r.get(7)?,
                    last_seq: r.get(8)?,
                    last_read_seq: r.get(9)?,
                    hitl_pending: r.get::<_, i64>(10)? != 0,
                    inbound_seqs: r.get(11)?,
                    ordinal: r.get(12)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // `ordinal` is NOT NULL UNIQUE and only ever written as MAX+1. A
        // negative one is corruption, and every caller narrows this field to
        // `u64` — so it must not escape the index as a plausible-looking
        // number.
        if let Some(bad) = rows.iter().find(|r| r.ordinal < 0) {
            anyhow::bail!(
                "channel {} has a corrupt ordinal in the index: {}",
                bad.id,
                bad.ordinal
            );
        }
        Ok(rows)
    }

    /// Delete the read-model row for `id`, plus its `channel_fts` body
    /// rows — `channel_fts` is a standalone FTS5 table with its own copy
    /// of the message text, not a view over `channels`, so it survives a
    /// bare `DELETE FROM channels` and would otherwise keep a deleted
    /// conversation's text on disk indefinitely. Idempotent — no error if
    /// not found.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM channels WHERE id = ?1", [id])
            .context("remove channel row")?;
        self.conn
            .execute("DELETE FROM channel_fts WHERE channel_id = ?1", [id])
            .context("remove channel fts rows")?;
        // A restarted chain reuses seqs from 0; stale ledger rows would
        // make the fresh chain's events look already folded.
        self.conn
            .execute("DELETE FROM channel_folds WHERE channel_id = ?1", [id])
            .context("remove channel fold rows")?;
        Ok(())
    }

    /// Drop every derived row and re-derive from manifests + event logs.
    ///
    /// Read watermarks are the exception: nothing in the event log records what
    /// a human has looked at, so they are carried across rather than lost.
    ///
    /// All-or-nothing: everything runs inside one `BEGIN IMMEDIATE` /
    /// `COMMIT` (`write_txn`), so a hard failure partway through the replay
    /// loop rolls back to the previous index instead of leaving it truncated
    /// (already-deleted rows, only some channels re-inserted). A successful
    /// rebuild also pays any rebuild owed by `migrate()`, in the same commit.
    pub fn rebuild_from(&self, store: &ChannelStore) -> Result<usize> {
        self.write_txn("rebuild", || self.rebuild_locked(store))
    }

    /// The body of `rebuild_from`; the caller holds the write transaction.
    fn rebuild_locked(&self, store: &ChannelStore) -> Result<usize> {
        let watermarks: Vec<(String, i64)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, last_read_seq FROM channels WHERE last_read_seq > 0")?;
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };

        self.conn.execute_batch(
            "DELETE FROM channels; DELETE FROM channel_fts; DELETE FROM channel_folds;",
        )?;

        let mut n = 0;
        // Sorted by creation time, not `list_ids()`'s `read_dir` order:
        // when the index file itself is lost, `channel_ordinals` is empty
        // too and this loop hands out every number from scratch. Directory
        // order is the filesystem's business, so without this the numbers
        // a user memorised come back shuffled. `id` breaks ties so the
        // result is deterministic for manifests written in the same tick.
        let mut manifests: Vec<Channel> = store
            .list_ids()?
            .into_iter()
            .filter_map(|id| store.load_manifest(&id).ok())
            .collect();
        manifests.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        for ch in manifests {
            let id = ch.id.clone();
            self.upsert(&ch)?;
            for ev in store.load_events(&id).unwrap_or_default() {
                self.record_event(&id, &ev)?;
            }
            n += 1;
        }

        for (id, seq) in watermarks {
            self.mark_read(&id, seq as u64)?;
        }
        self.clear_rebuild_pending()?;
        Ok(n)
    }

    /// Raw connection, for tests that need to simulate out-of-band writes.
    #[cfg(test)]
    pub(crate) fn conn_for_test(&self) -> &Connection {
        &self.conn
    }
}

mod pending;
#[cfg(test)]
mod pending_tests;
#[cfg(test)]
mod tests;
