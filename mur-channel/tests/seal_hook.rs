//! Contract tests for the sealing hook on `ChannelStore::append_event_with`
//! (channel-log-integrity spec 7.3.1, decisions B1 and C1). This slice covers
//! the hook SHAPE only: when it runs, what it sees, and what its failure
//! means. Head files and chain hashes are a later slice.

use std::cell::{Cell, RefCell};

use chrono::Utc;
use fs2::FileExt;
use mur_channel::ChannelStore;
use mur_common::channel::{Channel, ChannelActor, ChannelState, EventKind, Goal};
use tempfile::TempDir;

const CH: &str = "seal-ch";

fn human() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

fn store_with_channel() -> (TempDir, ChannelStore) {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let now = Utc::now();
    store
        .create(&Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: CH.into(),
            title: "t".into(),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: None,
            owner: human(),
            participants: vec![],
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    (tmp, store)
}

fn append(
    store: &ChannelStore,
    text: &str,
    key: Option<&str>,
    seal: Option<&mur_channel::store::SealHook<'_>>,
) -> anyhow::Result<mur_common::channel::ChannelEvent> {
    store.append_event_with(
        CH,
        human(),
        EventKind::Message,
        serde_json::json!({ "text": text }),
        key.map(str::to_string),
        None,
        None,
        mur_channel::store::SealHooks::post(seal),
    )
}

/// B1: a failed seal is an `Err`, never converted to success, and the line
/// that was already written stays on disk.
#[test]
fn failed_seal_returns_err_and_the_event_stays_on_disk() {
    let (_tmp, store) = store_with_channel();
    let fail = |_: &mur_channel::store::SealInput| -> anyhow::Result<()> {
        anyhow::bail!("seal exploded")
    };
    let err = append(&store, "hello", None, Some(&fail)).unwrap_err();
    assert!(format!("{err:#}").contains("seal exploded"), "{err:#}");

    let events = store.load_events(CH).unwrap();
    assert_eq!(events.len(), 1, "event must be on disk after a failed seal");
    assert_eq!(events[0].payload["text"], "hello");
}

/// The hook sees exactly what was written: the event (with its assigned seq),
/// the channel id, and the record bytes as they sit in events.jsonl.
#[test]
fn hook_receives_the_event_channel_id_and_exact_record_bytes() {
    let (_tmp, store) = store_with_channel();
    let seen: RefCell<Vec<(u64, String, Vec<u8>)>> = RefCell::new(vec![]);
    let hook = |s: &mur_channel::store::SealInput| -> anyhow::Result<()> {
        seen.borrow_mut()
            .push((s.event.seq, s.channel_id.to_string(), s.record.to_vec()));
        Ok(())
    };
    append(&store, "a", None, Some(&hook)).unwrap();
    append(&store, "b", None, Some(&hook)).unwrap();

    let seen = seen.into_inner();
    assert_eq!(seen.len(), 2, "hook runs once per written line");
    assert_eq!((seen[0].0, seen[1].0), (0, 1));
    assert!(seen.iter().all(|s| s.1 == CH));

    let on_disk = std::fs::read(store.events_path(CH).unwrap()).unwrap();
    let joined: Vec<u8> = seen.iter().flat_map(|s| s.2.clone()).collect();
    assert_eq!(joined, on_disk, "record bytes must equal the log bytes");
    assert!(seen[0].2.ends_with(b"\n"), "record includes its terminator");
}

/// A dedup hit writes nothing, but it does re-offer the line to the hook (the
/// hook owns idempotency: head already covers the seq => no-op).
#[test]
fn dedup_hit_reruns_the_hook_but_appends_nothing() {
    let (_tmp, store) = store_with_channel();
    let calls = Cell::new(0);
    let hook = |_: &mur_channel::store::SealInput| -> anyhow::Result<()> {
        calls.set(calls.get() + 1);
        Ok(())
    };
    let first = append(&store, "once", Some("k1"), Some(&hook)).unwrap();
    let again = append(&store, "once", Some("k1"), Some(&hook)).unwrap();
    assert_eq!(first.seq, again.seq);
    assert_eq!(calls.get(), 2, "dedup hit re-offers the line to the hook");
    assert_eq!(store.load_events(CH).unwrap().len(), 1);
}

/// 7.3.1: the hook runs after the write and BEFORE the unlock — a second
/// handle on events.lock must not be able to take it from inside the hook,
/// and must be able to once the append has returned.
#[test]
fn hook_runs_while_events_lock_is_still_held() {
    let (_tmp, store) = store_with_channel();
    let lock_path = store
        .events_path(CH)
        .unwrap()
        .parent()
        .unwrap()
        .join("events.lock");
    let contended = Cell::new(None);
    let hook = |_: &mur_channel::store::SealInput| -> anyhow::Result<()> {
        let other = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        contended.set(Some(other.try_lock_exclusive().is_err()));
        Ok(())
    };
    append(&store, "x", None, Some(&hook)).unwrap();
    assert_eq!(contended.get(), Some(true), "lock must be held in the hook");

    let after = std::fs::OpenOptions::new()
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(
        after.try_lock_exclusive().is_ok(),
        "lock released afterwards"
    );
}

/// A failing hook must not leave the lock held: the next append succeeds.
#[test]
fn failed_seal_releases_the_lock() {
    let (_tmp, store) = store_with_channel();
    let fail = |_: &mur_channel::store::SealInput| -> anyhow::Result<()> { anyhow::bail!("no") };
    append(&store, "one", None, Some(&fail)).unwrap_err();
    let ev = append(&store, "two", None, None).unwrap();
    assert_eq!(ev.seq, 1, "seq continues after the unsealed line");
}

/// `seal = None` is the legacy path: same on-disk effect as `append_event`.
#[test]
fn none_seal_matches_the_legacy_append_event() {
    let (_tmp, store) = store_with_channel();
    let new = append(&store, "same", None, None).unwrap();
    let old = store
        .append_event(
            CH,
            human(),
            EventKind::Message,
            serde_json::json!({ "text": "same" }),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!((new.seq, old.seq), (0, 1));
    assert_eq!(new.payload, old.payload);
    let lines = std::fs::read_to_string(store.events_path(CH).unwrap()).unwrap();
    assert_eq!(lines.lines().count(), 2);
    assert!(lines.ends_with('\n'));
}

/// A dedup hit re-runs the seal hook: after a failed seal the line is on
/// disk, and the retry with the same idempotency key must give the hook
/// another chance to seal it. The hook owns idempotency (head already covers
/// the seq => no-op), so the store always calls it on a hit.
#[test]
fn retry_after_failed_seal_is_deduped_and_resealed() {
    let (_tmp, store) = store_with_channel();
    let calls = Cell::new(0u32);
    let seen = RefCell::new(Vec::<(u64, Vec<u8>)>::new());
    let flaky = |i: &mur_channel::store::SealInput<'_>| -> anyhow::Result<()> {
        calls.set(calls.get() + 1);
        seen.borrow_mut().push((i.event.seq, i.record.to_vec()));
        if calls.get() == 1 {
            anyhow::bail!("disk full")
        }
        Ok(())
    };
    assert!(append(&store, "a", Some("k"), Some(&flaky)).is_err());
    assert_eq!(calls.get(), 1);

    let retry = append(&store, "a", Some("k"), Some(&flaky)).expect("second seal succeeds");
    assert_eq!(retry.seq, 0, "dedup returns the original event");
    assert_eq!(calls.get(), 2, "hook ran again on the dedup hit");
    assert_eq!(
        store.load_events(CH).unwrap().len(),
        1,
        "nothing re-appended"
    );
    let seen = seen.borrow();
    assert_eq!(
        seen[0], seen[1],
        "same seq and exact record bytes both times"
    );
    let on_disk = std::fs::read(store.events_path(CH).unwrap()).unwrap();
    assert_eq!(seen[1].1, on_disk, "record is the exact line on disk");
}

/// A hook that fails again on the retry surfaces as `Err`, same as the write path.
#[test]
fn retry_after_failed_seal_errs_when_the_hook_fails_again() {
    let (_tmp, store) = store_with_channel();
    let calls = Cell::new(0u32);
    let failing = |_: &mur_channel::store::SealInput<'_>| -> anyhow::Result<()> {
        calls.set(calls.get() + 1);
        anyhow::bail!("disk full")
    };
    assert!(append(&store, "a", Some("k"), Some(&failing)).is_err());
    let retry = append(&store, "a", Some("k"), Some(&failing));
    let err = format!("{:#}", retry.expect_err("second failure is an Err"));
    assert!(
        err.contains("seal failed after the event was written"),
        "{err}"
    );
    assert_eq!(calls.get(), 2);
    assert_eq!(store.load_events(CH).unwrap().len(), 1);
}

/// `seal: None` keeps the legacy dedup: nothing re-read, nothing sealed.
#[test]
fn dedup_with_no_hook_is_unchanged() {
    let (_tmp, store) = store_with_channel();
    let first = append(&store, "a", Some("k"), None).unwrap();
    let again = append(&store, "a", Some("k"), None).unwrap();
    assert_eq!(first.seq, again.seq);
    assert_eq!(store.load_events(CH).unwrap().len(), 1);
}

/// A hand-edited (CRLF) log cannot yield the bytes that were originally
/// written, so re-sealing refuses instead of letting the hook hash a
/// divergent record. The hook must not run.
#[test]
fn dedup_reseal_refuses_a_crlf_line() {
    let (_tmp, store) = store_with_channel();
    append(&store, "a", Some("k"), None).unwrap();
    let path = store.events_path(CH).unwrap();
    let crlf = std::fs::read_to_string(&path)
        .unwrap()
        .replace('\n', "\r\n");
    std::fs::write(&path, crlf).unwrap();

    let calls = Cell::new(0u32);
    let hook = |_: &mur_channel::store::SealInput<'_>| -> anyhow::Result<()> {
        calls.set(calls.get() + 1);
        Ok(())
    };
    assert!(append(&store, "a", Some("k"), Some(&hook)).is_err());
    assert_eq!(
        calls.get(),
        0,
        "no hook call on a record we cannot reproduce"
    );
}

// ---- pre_write (#1770): the point before the line exists ----

fn lines(store: &ChannelStore) -> usize {
    std::fs::read_to_string(store.events_path(CH).unwrap())
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// `pre_write` runs under the lock, after `seq` is assigned and BEFORE the
/// write: it sees the final seq and an unchanged log; `post_write` follows.
#[test]
fn pre_write_runs_before_the_line_with_the_final_seq() {
    let (_t, store) = store_with_channel();
    append(&store, "a", None, None).unwrap();
    let order = RefCell::new(Vec::new());
    let pre = |i: &mur_channel::store::PreSealInput<'_>| {
        order
            .borrow_mut()
            .push(format!("pre seq={} lines={}", i.event.seq, lines(&store)));
        Ok(())
    };
    let post = |i: &mur_channel::store::SealInput<'_>| {
        order
            .borrow_mut()
            .push(format!("post seq={} lines={}", i.event.seq, lines(&store)));
        Ok(())
    };
    store
        .append_event_with(
            CH,
            human(),
            EventKind::Message,
            serde_json::json!({ "text": "b" }),
            None,
            None,
            None,
            mur_channel::store::SealHooks {
                pre_write: Some(&pre),
                post_write: Some(&post),
            },
        )
        .unwrap();
    assert_eq!(
        *order.borrow(),
        vec!["pre seq=1 lines=1".to_string(), "post seq=1 lines=2".into()]
    );
}

/// A failing `pre_write` aborts with NOTHING written and no `post_write`,
/// and the lock is released (the next append goes through).
#[test]
fn failed_pre_write_writes_nothing_and_skips_post() {
    let (_t, store) = store_with_channel();
    let posts = Cell::new(0);
    let pre = |_: &mur_channel::store::PreSealInput<'_>| anyhow::bail!("marker down");
    let post = |_: &mur_channel::store::SealInput<'_>| {
        posts.set(posts.get() + 1);
        Ok(())
    };
    let err = store
        .append_event_with(
            CH,
            human(),
            EventKind::Message,
            serde_json::json!({ "text": "x" }),
            None,
            None,
            None,
            mur_channel::store::SealHooks {
                pre_write: Some(&pre),
                post_write: Some(&post),
            },
        )
        .unwrap_err();
    assert!(format!("{err:#}").contains("marker down"), "{err:#}");
    assert_eq!(lines(&store), 0, "nothing written");
    assert_eq!(posts.get(), 0, "post_write never ran");
    let lock =
        std::fs::File::open(store.events_path(CH).unwrap().with_file_name("events.lock")).unwrap();
    lock.try_lock_exclusive().expect("lock released");
    FileExt::unlock(&lock).unwrap();
    append(&store, "y", None, None).unwrap();
    assert_eq!(lines(&store), 1);
}

/// A dedup hit writes nothing, so `pre_write` must not run (it would plant a
/// marker for a line that never comes); only `post_write` gets its retry.
#[test]
fn dedup_hit_skips_pre_write() {
    let (_t, store) = store_with_channel();
    append(&store, "a", Some("k"), None).unwrap();
    let pres = Cell::new(0);
    let pre = |_: &mur_channel::store::PreSealInput<'_>| {
        pres.set(pres.get() + 1);
        Ok(())
    };
    store
        .append_event_with(
            CH,
            human(),
            EventKind::Message,
            serde_json::json!({ "text": "a" }),
            Some("k".into()),
            None,
            None,
            mur_channel::store::SealHooks {
                pre_write: Some(&pre),
                post_write: None,
            },
        )
        .unwrap();
    assert_eq!(pres.get(), 0);
}
