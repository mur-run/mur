//! The next `seq` comes from every non-empty line, not only the lines this
//! binary can parse (channel-consult R7). An older writer that skipped a newer
//! `kind` would reuse its `seq`, and the index's `seq > last` guard would then
//! drop the older writer's event without a word.

use std::io::Write as _;

use chrono::Utc;
use mur_channel::ChannelStore;
use mur_common::channel::{Channel, ChannelActor, ChannelState, EventKind, Goal};
use tempfile::TempDir;

fn store_with_channel(id: &str) -> (TempDir, ChannelStore) {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let now = Utc::now();
    store
        .create(&Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: id.to_owned(),
            title: "t".into(),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: None,
            owner: ChannelActor::Human { name: "me".into() },
            participants: vec![],
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    (tmp, store)
}

fn append(store: &ChannelStore, id: &str) -> u64 {
    store
        .append_event(
            id,
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({ "text": "x" }),
            None,
            None,
            None,
        )
        .unwrap()
        .seq
}

fn append_raw(store: &ChannelStore, id: &str, line: &str) {
    let path = store.events_path(id).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{line}").unwrap();
}

/// A line a newer writer appended, with a `kind` this binary does not know.
fn unknown_kind_line(seq: u64) -> String {
    serde_json::json!({
        "seq": seq,
        "ts": Utc::now(),
        "actor": { "kind": "system" },
        "kind": "kind-from-the-future",
        "payload": {},
    })
    .to_string()
}

#[test]
fn a_trailing_line_of_unknown_kind_still_advances_seq() {
    let (_tmp, store) = store_with_channel("c1");
    assert_eq!(append(&store, "c1"), 0);
    append_raw(&store, "c1", &unknown_kind_line(7));

    assert_eq!(
        append(&store, "c1"),
        8,
        "the next seq must follow the unknown-kind line, not reuse its seq"
    );
}

#[test]
fn an_unknown_kind_line_alone_still_advances_seq() {
    let (_tmp, store) = store_with_channel("c1");
    append_raw(&store, "c1", &unknown_kind_line(7));

    assert_eq!(append(&store, "c1"), 8);
}

#[test]
fn a_line_with_no_seq_does_not_move_seq() {
    let (_tmp, store) = store_with_channel("c1");
    assert_eq!(append(&store, "c1"), 0);
    append_raw(&store, "c1", r#"{"kind":"message"}"#);
    append_raw(&store, "c1", "not json at all");

    assert_eq!(append(&store, "c1"), 1);
}

#[test]
fn the_unknown_kind_line_is_still_skipped_by_load() {
    let (_tmp, store) = store_with_channel("c1");
    append(&store, "c1");
    append_raw(&store, "c1", &unknown_kind_line(1));
    append(&store, "c1");

    let seqs: Vec<u64> = store
        .load_events("c1")
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(
        seqs,
        vec![0, 2],
        "readers still fold only the kinds they know"
    );
}
