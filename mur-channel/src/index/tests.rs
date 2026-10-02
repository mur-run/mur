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
fn rebuild_is_atomic_a_mid_loop_failure_leaves_the_previous_index_intact() {
    // A hard failure partway through the replay loop must not leave the
    // index in a truncated state (already-deleted rows, only some
    // channels re-inserted). Simulated with a trigger that aborts the
    // INSERT for channel `b` specifically — `a` and `c` bracket it so
    // regardless of `fs::read_dir`'s (unspecified) iteration order,
    // there is always at least one channel processed on either side of
    // the failure. The assertion does not depend on knowing which side:
    // a fully atomic rebuild rolls back to the exact pre-rebuild rows
    // no matter where in the loop it failed.
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    store.create(&ch("a", ChannelState::Working)).unwrap();
    store.create(&ch("b", ChannelState::Working)).unwrap();
    store.create(&ch("c", ChannelState::Working)).unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    let n = idx.rebuild_from(&store).unwrap();
    assert_eq!(
        n, 3,
        "sanity: all three channels present before the induced failure"
    );

    let before = idx.list(10).unwrap();
    assert_eq!(before.len(), 3);

    idx.conn_for_test()
        .execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON channels
                 WHEN NEW.id = 'b'
                 BEGIN SELECT RAISE(FAIL, 'induced failure'); END;",
        )
        .unwrap();

    let result = idx.rebuild_from(&store);
    assert!(
        result.is_err(),
        "the induced trigger failure must propagate as an error, not be swallowed"
    );

    idx.conn_for_test()
        .execute_batch("DROP TRIGGER boom")
        .unwrap();

    let mut after: Vec<String> = idx.list(10).unwrap().into_iter().map(|r| r.id).collect();
    let mut before_ids: Vec<String> = before.into_iter().map(|r| r.id).collect();
    after.sort();
    before_ids.sort();
    assert_eq!(
        after, before_ids,
        "a failed rebuild must roll back to the pre-rebuild rows, not a truncated set"
    );
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

fn fts_row_count(idx: &ChannelIndex, ch_id: &str) -> i64 {
    idx.conn_for_test()
        .query_row(
            "SELECT COUNT(*) FROM channel_fts WHERE channel_id = ?1",
            [ch_id],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn record_event_is_idempotent_for_a_replayed_idempotency_key() {
    // `ChannelStore::append_event` dedups on idempotency_key: a
    // crash-rerun with the same key returns the SAME pre-existing event
    // (same seq), not a new one. `ChannelService::append` (and siblings)
    // then unconditionally call `record_event` with whatever
    // `append_event` returned — so a rerun must not double-fold.
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    let c = ch("c1", ChannelState::Working);
    store.create(&c).unwrap();
    idx.upsert(&c).unwrap();

    let ev1 = store
        .append_event(
            "c1",
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            serde_json::json!({"text": "hello"}),
            Some("idem-1".into()),
            None,
            None,
        )
        .unwrap();
    idx.record_event("c1", &ev1).unwrap();

    let row = idx.list(10).unwrap().into_iter().next().unwrap();
    assert_eq!(row.msg_count, 1);
    assert_eq!(row.inbound_seqs, "[0]");
    assert_eq!(fts_row_count(&idx, "c1"), 1);

    // Simulate the crash-rerun: same idempotency_key, second call.
    let ev2 = store
        .append_event(
            "c1",
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            serde_json::json!({"text": "hello"}),
            Some("idem-1".into()),
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        ev2.seq, ev1.seq,
        "store-level dedup must return the same pre-existing event, not append a new one"
    );
    idx.record_event("c1", &ev2).unwrap();

    let row = idx.list(10).unwrap().into_iter().next().unwrap();
    assert_eq!(
        row.msg_count, 1,
        "replayed idempotency key must not double-count msg_count"
    );
    assert_eq!(
        row.inbound_seqs, "[0]",
        "replayed idempotency key must not duplicate the seq in inbound_seqs (the unread badge)"
    );
    assert_eq!(
        fts_row_count(&idx, "c1"),
        1,
        "replayed idempotency key must not duplicate the FTS row"
    );
}

#[test]
fn record_event_folds_the_channels_first_event_at_seq_zero() {
    // Regression test for the -1 sentinel: seqs are 0-indexed, so a
    // fresh row's last_seq must start at -1, not 0 — otherwise the
    // dedup guard `ev.seq > last_seq` rejects the channel's very first
    // event (seq 0) forever.
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    let c = ch("c1", ChannelState::Working);
    idx.upsert(&c).unwrap();
    assert_eq!(
        idx.list(10).unwrap()[0].last_seq,
        -1,
        "a fresh row must start at the sentinel, not 0"
    );

    let ev = ChannelEvent {
        seq: 0,
        ts: Utc::now(),
        actor: ChannelActor::Agent { id: "mur".into() },
        kind: EventKind::Message,
        payload: serde_json::json!({"text": "first"}),
        idempotency_key: None,
        sig: None,
        key_version: None,
    };
    idx.record_event("c1", &ev).unwrap();

    let row = idx.list(10).unwrap().into_iter().next().unwrap();
    assert_eq!(row.last_seq, 0, "seq 0 must fold, not be silently skipped");
    assert_eq!(row.msg_count, 1);
    assert_eq!(row.inbound_seqs, "[0]");
    assert_eq!(fts_row_count(&idx, "c1"), 1);
}

#[test]
fn remove_deletes_the_channels_fts_body_text_too() {
    // Regression: `remove()` used to delete only the `channels` row.
    // `channel_fts` is a standalone FTS5 table with its own copy of the
    // message text, not a view over `channels`, so the body text
    // survived on disk indefinitely after a user deleted the
    // conversation. `search_bodies` already can't see the orphan (its
    // query `JOIN`s `channels`, so a gone channel id yields zero rows
    // for that reason alone) — that's the review's "search isn't
    // wrong" observation, but it also means `search_bodies` can't
    // discriminate this bug either way. The row-count check on
    // `channel_fts` directly is what actually proves the text is gone,
    // not merely unreachable via one query path.
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    let c = ch("c1", ChannelState::Working);
    idx.upsert(&c).unwrap();
    let ev = ChannelEvent {
        seq: 0,
        ts: Utc::now(),
        actor: ChannelActor::Agent { id: "mur".into() },
        kind: EventKind::Message,
        payload: serde_json::json!({"text": "unobtainium secret plans"}),
        idempotency_key: None,
        sig: None,
        key_version: None,
    };
    idx.record_event("c1", &ev).unwrap();
    assert_eq!(
        fts_row_count(&idx, "c1"),
        1,
        "sanity: indexed before removal"
    );
    assert_eq!(
        idx.search_bodies("unobtainium", 10).unwrap().len(),
        1,
        "sanity: findable before removal"
    );

    idx.remove("c1").unwrap();

    assert_eq!(
        fts_row_count(&idx, "c1"),
        0,
        "channel_fts rows must be deleted alongside the channels row, \
             not merely hidden by search_bodies's JOIN"
    );
    assert!(idx.search_bodies("unobtainium", 10).unwrap().is_empty());
}

fn ordinal_of(idx: &ChannelIndex, id: &str) -> i64 {
    idx.list(50)
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("channel {id} not indexed"))
        .ordinal
}

#[test]
fn ordinals_are_handed_out_in_creation_order_starting_at_one() {
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("a", ChannelState::Working)).unwrap();
    idx.upsert(&ch("b", ChannelState::Working)).unwrap();
    idx.upsert(&ch("c", ChannelState::Working)).unwrap();
    assert_eq!(ordinal_of(&idx, "a"), 1);
    assert_eq!(ordinal_of(&idx, "b"), 2);
    assert_eq!(ordinal_of(&idx, "c"), 3);
}

#[test]
fn re_upserting_a_channel_keeps_its_original_number() {
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("a", ChannelState::Working)).unwrap();
    idx.upsert(&ch("b", ChannelState::Working)).unwrap();
    idx.upsert(&ch("a", ChannelState::Completed)).unwrap();
    assert_eq!(
        ordinal_of(&idx, "a"),
        1,
        "an update must not renumber the channel the user is looking at"
    );
    assert_eq!(ordinal_of(&idx, "b"), 2);
}

#[test]
fn a_deleted_channels_number_is_burned_not_reused() {
    // The whole point of a stable number is that the `2` a user typed
    // yesterday never silently means a different conversation today.
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("a", ChannelState::Working)).unwrap();
    idx.upsert(&ch("b", ChannelState::Working)).unwrap();
    idx.remove("b").unwrap();
    idx.upsert(&ch("c", ChannelState::Working)).unwrap();
    assert_eq!(ordinal_of(&idx, "a"), 1);
    assert_eq!(
        ordinal_of(&idx, "c"),
        3,
        "the new channel must take the next number, never the dead one's"
    );
}

#[test]
fn rebuilding_the_index_does_not_renumber_channels() {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    store.create(&ch("a", ChannelState::Working)).unwrap();
    store.create(&ch("b", ChannelState::Working)).unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.rebuild_from(&store).unwrap();
    let before: Vec<(String, i64)> = idx
        .list(50)
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.ordinal))
        .collect();
    assert!(before.iter().all(|(_, n)| *n > 0), "sanity: numbered");

    idx.rebuild_from(&store).unwrap();

    let after: Vec<(String, i64)> = idx
        .list(50)
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.ordinal))
        .collect();
    assert_eq!(
        after, before,
        "rebuild_from wipes `channels`; the numbers live elsewhere and must survive it"
    );
}

#[test]
fn a_rebuild_from_an_empty_index_numbers_by_creation_time_not_directory_order() {
    // The disaster case the other rebuild test does not cover: the whole
    // index file is gone, so `channel_ordinals` is empty too and every
    // number is handed out fresh. `list_ids` is `read_dir` order, which is
    // the filesystem's business, not creation order — without an explicit
    // sort the numbers come out shuffled.
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    // Created oldest-first: z, m, a. Named so that *any* name-based or
    // inode-based ordering disagrees with the answer we want.
    for (id, mins) in [("zebra", 30_i64), ("mango", 20), ("apple", 10)] {
        let mut c = ch(id, ChannelState::Working);
        c.created_at = Utc::now() - chrono::Duration::minutes(mins);
        store.create(&c).unwrap();
    }
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.rebuild_from(&store).unwrap();
    assert_eq!(ordinal_of(&idx, "zebra"), 1, "oldest channel is #1");
    assert_eq!(ordinal_of(&idx, "mango"), 2);
    assert_eq!(ordinal_of(&idx, "apple"), 3);
}

#[test]
fn list_refuses_a_row_whose_ordinal_is_negative() {
    // Same corruption signal as `ordinal_of`, on the path the CLI listing
    // actually uses. Callers convert this field to `u64`; if the listing
    // hands back a clamped 0 they render it as "unnumbered" and the
    // corruption is never seen by anyone.
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("a", ChannelState::Working)).unwrap();
    idx.conn_for_test()
        .execute(
            "UPDATE channel_ordinals SET ordinal = -7 WHERE id = 'a'",
            [],
        )
        .unwrap();

    let err = idx
        .list(50)
        .expect_err("a negative ordinal must surface as an error");
    assert!(
        err.to_string().contains("-7"),
        "the error must name the bad value, got: {err}"
    );
}

#[test]
fn a_negative_ordinal_is_reported_not_clamped_to_zero() {
    // `ordinal` is NOT NULL UNIQUE and only ever written as MAX+1, so a
    // negative value means the DB is corrupt. Clamping it to 0 would hand
    // back the same value that legitimately means "not numbered yet",
    // laundering a corruption signal into a valid-looking answer.
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("a", ChannelState::Working)).unwrap();
    idx.conn_for_test()
        .execute(
            "UPDATE channel_ordinals SET ordinal = -3 WHERE id = 'a'",
            [],
        )
        .unwrap();

    let err = idx
        .ordinal_of("a")
        .expect_err("a negative ordinal must surface as an error");
    assert!(
        err.to_string().contains("-3"),
        "the error must name the bad value, got: {err}"
    );
}

#[test]
fn an_index_predating_the_ordinals_table_is_backfilled_in_creation_order() {
    let tmp = TempDir::new().unwrap();
    let idx = ChannelIndex::open(tmp.path()).unwrap();
    idx.upsert(&ch("a", ChannelState::Working)).unwrap();
    idx.upsert(&ch("b", ChannelState::Working)).unwrap();
    // Stand in for a DB written before the table existed.
    idx.conn_for_test()
        .execute_batch("DROP TABLE channel_ordinals")
        .unwrap();
    drop(idx);

    let idx = ChannelIndex::open(tmp.path()).unwrap();
    assert_eq!(ordinal_of(&idx, "a"), 1);
    assert_eq!(ordinal_of(&idx, "b"), 2);
}
