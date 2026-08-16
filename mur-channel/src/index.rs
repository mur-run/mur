use std::path::Path;

use anyhow::{Context, Result};
use mur_common::channel::{Channel, ChannelActor, ChannelEvent, EventKind};
use rusqlite::Connection;

use crate::store::ChannelStore;

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
        // Additive columns. `ADD COLUMN` on an existing column errors; that is
        // the "already migrated" case, so it is ignored deliberately.
        for ddl in [
            "ALTER TABLE channels ADD COLUMN purpose TEXT NOT NULL DEFAULT 'conversation'",
            "ALTER TABLE channels ADD COLUMN agents TEXT NOT NULL DEFAULT '[]'",
            "ALTER TABLE channels ADD COLUMN preview TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE channels ADD COLUMN msg_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE channels ADD COLUMN last_seq INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE channels ADD COLUMN last_read_seq INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE channels ADD COLUMN hitl_pending INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE channels ADD COLUMN inbound_seqs TEXT NOT NULL DEFAULT '[]'",
        ] {
            let _ = self.conn.execute(ddl, []);
        }
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_channels_purpose ON channels(purpose, updated_at DESC);",
        )?;
        // Rebuildable full-text projection of message bodies. `content=''` keeps
        // FTS5 from storing a second copy of the text it indexes.
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
        self.conn.execute(
            "INSERT INTO channels (id,title,state,owner,created_at,updated_at,purpose,agents)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
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
        let hitl_delta = match ev.kind {
            EventKind::HitlRequest => Some(1_i64),
            EventKind::HitlResponse => Some(0_i64),
            _ => None,
        };
        let inbound = counts && !matches!(ev.actor, ChannelActor::Human { .. });
        self.conn.execute(
            "UPDATE channels SET
               last_seq  = MAX(last_seq, ?2),
               msg_count = msg_count + ?3,
               preview   = CASE WHEN ?3 = 1 THEN ?4 ELSE preview END,
               hitl_pending = COALESCE(?5, hitl_pending),
               inbound_seqs = CASE WHEN ?6 = 1
                   THEN json_insert(inbound_seqs, '$[#]', ?2)
                   ELSE inbound_seqs END
             WHERE id = ?1",
            rusqlite::params![
                ch_id,
                ev.seq as i64,
                if counts { 1_i64 } else { 0_i64 },
                preview,
                hitl_delta,
                if inbound { 1_i64 } else { 0_i64 },
            ],
        )?;
        if counts && !preview.is_empty() {
            self.conn.execute(
                "INSERT INTO channel_fts (channel_id, seq, body) VALUES (?1, ?2, ?3)",
                rusqlite::params![ch_id, ev.seq as i64, preview],
            )?;
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

    /// Newest-first channel list (the Hub left-rail / CLI "my work" inbox).
    pub fn list(&self, limit: usize) -> Result<Vec<ChannelRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,title,state,updated_at,purpose,agents,preview,msg_count,last_seq,last_read_seq,hitl_pending,inbound_seqs
             FROM channels ORDER BY updated_at DESC, rowid DESC LIMIT ?1",
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
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Delete the read-model row for `id`. Idempotent — no error if not found.
    pub fn remove(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM channels WHERE id = ?1", [id])
            .context("remove channel row")?;
        Ok(())
    }

    /// Drop every row and re-derive from the store's manifests.
    pub fn rebuild_from(&self, store: &ChannelStore) -> Result<usize> {
        self.conn.execute("DELETE FROM channels", [])?;
        let mut n = 0;
        for id in store.list_ids()? {
            if let Ok(ch) = store.load_manifest(&id) {
                self.upsert(&ch)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Raw connection, for tests that need to simulate out-of-band writes.
    #[cfg(test)]
    pub(crate) fn conn_for_test(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::channel::{ChannelActor, ChannelState, Goal, Participant, ParticipantRole};
    use tempfile::TempDir;

    fn ch(id: &str, state: ChannelState) -> Channel {
        let now = Utc::now();
        Channel {
            v: 1,
            id: id.into(),
            title: id.into(),
            goal: Goal::default(),
            state,
            purpose: None,
            owner: ChannelActor::Human { name: "me".into() },
            participants: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn upsert_and_list_newest_first() {
        let tmp = TempDir::new().unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        idx.upsert(&ch("a", ChannelState::Working)).unwrap();
        idx.upsert(&ch("b", ChannelState::Completed)).unwrap();
        let rows = idx.list(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].state, "completed"); // serialized kebab, quotes trimmed
    }

    #[test]
    fn rebuild_from_store_repopulates() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&ch("a", ChannelState::Working)).unwrap();
        store.create(&ch("b", ChannelState::Failed)).unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        assert_eq!(idx.list(10).unwrap().len(), 0);
        let n = idx.rebuild_from(&store).unwrap();
        assert_eq!(n, 2);
        assert_eq!(idx.list(10).unwrap().len(), 2);
    }

    #[test]
    fn open_migrates_old_layout_db_into_channels_subdir() {
        let tmp = TempDir::new().unwrap();
        let old_dir = tmp.path().join("index");
        std::fs::create_dir_all(&old_dir).unwrap();
        let old_db = old_dir.join("channels.db");
        // Empty file: SQLite treats a zero-length file as a valid new,
        // empty database (no fixed header requirement), so this stands in
        // for a real old-layout channels.db without depending on the
        // on-disk SQLite format.
        std::fs::write(&old_db, b"").unwrap();

        ChannelIndex::open(tmp.path()).unwrap();

        let new_db = old_dir.join("channels").join("channels.db");
        assert!(new_db.exists(), "db must be migrated to the new subdir");
        assert!(
            !old_db.exists(),
            "old-location file must be gone after migration"
        );
    }

    #[test]
    fn migrate_adds_columns_to_a_preexisting_v1_database() {
        // Simulate a DB created before these columns existed, then open the
        // index over it. ALTER TABLE must run without destroying rows.
        let tmp = TempDir::new().unwrap();
        // The index lives at <mur_home>/index/channels/channels.db — NOT
        // alongside channel data in <mur_home>/channels/.
        let dir = tmp.path().join("index").join("channels");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("channels.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE channels (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
                    owner TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO channels VALUES ('old','chat with mur','working','{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        let idx = ChannelIndex::open(tmp.path()).expect("must migrate, not fail");
        let rows = idx.list(10).unwrap();
        assert_eq!(rows.len(), 1, "existing row must survive migration");
        assert_eq!(rows[0].id, "old");
        assert_eq!(
            rows[0].purpose, "conversation",
            "column DEFAULT until something re-upserts it"
        );
    }

    #[test]
    fn upsert_writes_purpose_and_agents() {
        let tmp = TempDir::new().unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        let mut c = ch("c1", ChannelState::Working);
        c.purpose = Some(mur_common::channel::ChannelPurpose::Conversation);
        c.participants = vec![Participant {
            actor: ChannelActor::Agent { id: "mur".into() },
            role: ParticipantRole::Delegate,
            joined_at: Utc::now(),
        }];
        idx.upsert(&c).unwrap();

        let rows = idx.list(10).unwrap();
        assert_eq!(rows[0].purpose, "conversation");
        assert_eq!(rows[0].agents, r#"["mur"]"#);
    }

    #[test]
    fn upsert_infers_purpose_for_a_legacy_manifest() {
        let tmp = TempDir::new().unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        let mut c = ch("fleet-projectx", ChannelState::Working);
        c.purpose = None; // legacy
        idx.upsert(&c).unwrap();
        assert_eq!(idx.list(10).unwrap()[0].purpose, "fleet-run");
    }

    #[test]
    fn upsert_does_not_clobber_activity_columns() {
        // Re-upserting a manifest (e.g. a state transition) must not reset the
        // preview/counters that the append path maintains.
        let tmp = TempDir::new().unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        let c = ch("c1", ChannelState::Working);
        idx.upsert(&c).unwrap();
        idx.conn_for_test()
            .execute(
                "UPDATE channels SET preview='hello', msg_count=3, last_seq=7 WHERE id='c1'",
                [],
            )
            .unwrap();

        idx.upsert(&c).unwrap();

        let r = &idx.list(10).unwrap()[0];
        assert_eq!(r.preview, "hello");
        assert_eq!(r.msg_count, 3);
        assert_eq!(r.last_seq, 7);
    }
}
