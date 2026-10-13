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
        seal,
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

    let on_disk = std::fs::read(store.events_path(CH)).unwrap();
    let joined: Vec<u8> = seen.iter().flat_map(|s| s.2.clone()).collect();
    assert_eq!(joined, on_disk, "record bytes must equal the log bytes");
    assert!(seen[0].2.ends_with(b"\n"), "record includes its terminator");
}

/// 7.3.1: the dedup path writes nothing, so it must not run the hook (and so
/// cannot advance a head).
#[test]
fn dedup_return_does_not_run_the_hook() {
    let (_tmp, store) = store_with_channel();
    let calls = Cell::new(0);
    let hook = |_: &mur_channel::store::SealInput| -> anyhow::Result<()> {
        calls.set(calls.get() + 1);
        Ok(())
    };
    let first = append(&store, "once", Some("k1"), Some(&hook)).unwrap();
    let again = append(&store, "once", Some("k1"), Some(&hook)).unwrap();
    assert_eq!(first.seq, again.seq);
    assert_eq!(calls.get(), 1, "dedup hit must not seal");
    assert_eq!(store.load_events(CH).unwrap().len(), 1);
}

/// 7.3.1: the hook runs after the write and BEFORE the unlock — a second
/// handle on events.lock must not be able to take it from inside the hook,
/// and must be able to once the append has returned.
#[test]
fn hook_runs_while_events_lock_is_still_held() {
    let (_tmp, store) = store_with_channel();
    let lock_path = store.events_path(CH).parent().unwrap().join("events.lock");
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
    let lines = std::fs::read_to_string(store.events_path(CH)).unwrap();
    assert_eq!(lines.lines().count(), 2);
    assert!(lines.ends_with('\n'));
}
