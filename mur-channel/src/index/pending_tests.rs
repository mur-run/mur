//! The post-migration rebuild is a durable obligation, not a one-shot: a
//! rebuild that fails must be retried on a later open, and a DB that was
//! left with folded rows but no fold ledger must heal itself.

use super::*;
use crate::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use tempfile::TempDir;

fn db_path(home: &Path) -> std::path::PathBuf {
    home.join("index").join("channels").join("channels.db")
}

fn raw(home: &Path) -> Connection {
    Connection::open(db_path(home)).unwrap()
}

/// A legacy install: one channel with one agent message on disk, and an
/// index that only has the base v1 schema — plus, when `poisoned`, a
/// trigger that makes every re-insert into `channels` fail, so the
/// post-migration rebuild fails partway.
fn legacy_install(home: &Path, poisoned: bool) -> String {
    let svc = ChannelService::open(home).unwrap();
    let id = svc.create_for_agent("mur").unwrap().id;
    svc.append_message(
        &id,
        ChannelActor::Agent { id: "mur".into() },
        EventKind::Message,
        "from before the upgrade",
        None,
    )
    .unwrap();
    drop(svc);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", db_path(home).display()));
    }
    let conn = raw(home);
    conn.execute_batch(
        "CREATE TABLE channels (
            id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
            owner TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
    )
    .unwrap();
    if poisoned {
        conn.execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON channels
             BEGIN SELECT RAISE(FAIL, 'induced rebuild failure'); END;",
        )
        .unwrap();
    }
    id
}

#[test]
fn a_failed_post_migration_rebuild_is_retried_on_the_next_open() {
    let tmp = TempDir::new().unwrap();
    legacy_install(tmp.path(), true);

    let svc = ChannelService::open(tmp.path()).expect("a failed rebuild must not block open");
    assert!(
        svc.index().rebuild_pending().unwrap(),
        "the rebuild failed, so it is still owed"
    );
    drop(svc);
    raw(tmp.path()).execute_batch("DROP TRIGGER boom").unwrap();

    let svc = ChannelService::open(tmp.path()).unwrap();
    let rows = svc.index().list(10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].msg_count, 1, "the retry must replay the event log");
    assert_eq!(rows[0].preview, "from before the upgrade");
    assert!(!svc.index().rebuild_pending().unwrap());
}

#[test]
fn a_successful_rebuild_is_not_repeated() {
    let tmp = TempDir::new().unwrap();
    let id = legacy_install(tmp.path(), false);
    drop(ChannelService::open(tmp.path()).unwrap());
    raw(tmp.path())
        .execute(
            "UPDATE channels SET preview = 'DIVERGED' WHERE id = ?1",
            [&id],
        )
        .unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    assert_eq!(svc.index().list(10).unwrap()[0].preview, "DIVERGED");
}

#[test]
fn folded_rows_without_a_ledger_heal_on_open_so_a_rerun_is_not_double_counted() {
    // The state a DB is left in when the ledger migration ran but its
    // rebuild never completed: rows folded, ledger empty, marker present.
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = svc.create_for_agent("mur").unwrap().id;
    let ev = svc
        .append_message(
            &id,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            "hello",
            None,
        )
        .unwrap();
    let before = svc.index().list(10).unwrap().remove(0);
    assert_eq!(before.msg_count, 1);
    drop(svc);
    raw(tmp.path())
        .execute_batch("DELETE FROM channel_folds")
        .unwrap();

    let svc = ChannelService::open(tmp.path()).unwrap();
    svc.index().record_event(&id, &ev).unwrap();
    let row = svc.index().list(10).unwrap().remove(0);
    assert_eq!(row.msg_count, 1, "a rerun of an old seq must stay a no-op");
    assert_eq!(row.inbound_seqs, before.inbound_seqs);
    let fts: i64 = raw(tmp.path())
        .query_row("SELECT COUNT(*) FROM channel_fts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fts, 1, "no duplicate search row");
}

#[test]
fn a_rebuild_already_done_by_another_connection_is_skipped() {
    // The CLI, the Hub and parallel_jobs open one DB concurrently; each
    // sees the flag before any of them rebuilds. Only the first may run it.
    let tmp = TempDir::new().unwrap();
    let id = legacy_install(tmp.path(), false);
    let store = ChannelStore::new(tmp.path());
    let first = ChannelIndex::open(tmp.path()).unwrap();
    let second = ChannelIndex::open(tmp.path()).unwrap();
    assert!(first.rebuild_pending().unwrap() && second.rebuild_pending().unwrap());

    assert_eq!(first.rebuild_if_pending(&store).unwrap(), Some(1));
    first
        .conn_for_test()
        .execute(
            "UPDATE channels SET preview = 'DIVERGED' WHERE id = ?1",
            [&id],
        )
        .unwrap();
    assert_eq!(second.rebuild_if_pending(&store).unwrap(), None);
    assert_eq!(second.list(10).unwrap()[0].preview, "DIVERGED");
}

#[test]
fn an_explicit_reindex_clears_the_flag() {
    let tmp = TempDir::new().unwrap();
    legacy_install(tmp.path(), false);
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    assert!(idx.rebuild_pending().unwrap());
    idx.rebuild_from(&ChannelStore::new(tmp.path())).unwrap();
    assert!(!idx.rebuild_pending().unwrap());
}
