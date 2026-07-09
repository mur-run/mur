use std::path::Path;

use anyhow::{Context, Result};
use mur_common::channel::Channel;
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
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .context("configure channels.db concurrency pragmas")?;
        let me = Self { conn };
        me.migrate()?;
        Ok(me)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS channels (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL,
                state       TEXT NOT NULL,
                owner       TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_channels_updated ON channels(updated_at DESC);",
        )?;
        Ok(())
    }

    pub fn upsert(&self, ch: &Channel) -> Result<()> {
        let owner = serde_json::to_string(&ch.owner)?;
        self.conn.execute(
            "INSERT INTO channels (id,title,state,owner,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(id) DO UPDATE SET
               title=excluded.title, state=excluded.state,
               owner=excluded.owner, updated_at=excluded.updated_at",
            rusqlite::params![
                ch.id,
                ch.title,
                serde_json::to_string(&ch.state)?.trim_matches('"'),
                owner,
                ch.created_at.to_rfc3339(),
                ch.updated_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Newest-first channel list (the Hub left-rail / CLI "my work" inbox).
    pub fn list(&self, limit: usize) -> Result<Vec<ChannelRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,title,state,updated_at FROM channels ORDER BY updated_at DESC, rowid DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit as i64], |r| {
                Ok(ChannelRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    state: r.get(2)?,
                    updated_at: r.get(3)?,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::channel::{ChannelActor, ChannelState, Goal};
    use tempfile::TempDir;

    fn ch(id: &str, state: ChannelState) -> Channel {
        let now = Utc::now();
        Channel {
            v: 1,
            id: id.into(),
            title: id.into(),
            goal: Goal::default(),
            state,
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
}
