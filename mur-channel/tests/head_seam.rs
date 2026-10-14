//! Characterization of the seam the head slice (#1770) must honour: the hook's
//! `record` carries the line terminator, and the line that gets chained is the
//! record with that terminator stripped. The next v2 event's `prev_hash` is the
//! single-line `line_hash` of exactly that stripped line.
//!
//! This pins the strip rule only. It does NOT pin the head's `chain_hash`: per
//! spec 7.2 that is the cumulative `H_n = SHA-256(H_{n-1} || raw_n)` over every
//! raw line, which is a different value from `prev_hash`. When the head folds
//! lines it must strip the terminator the same way this test shows.

use std::cell::RefCell;

use chrono::Utc;
use mur_channel::ChannelStore;
use mur_channel::store::{GENESIS_PREV_HASH, SealHooks, SealInput, line_hash};
use mur_common::channel::{Channel, ChannelActor, ChannelState, EventKind, Goal};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

const CH: &str = "head-seam-ch";
const N: usize = 3;

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

#[test]
fn sealed_record_minus_newline_is_the_chained_line() {
    let (_tmp, store) = store_with_channel();
    let records: RefCell<Vec<Vec<u8>>> = RefCell::new(vec![]);
    let record_hook = |input: &SealInput| -> anyhow::Result<()> {
        records.borrow_mut().push(input.record.to_vec());
        Ok(())
    };

    for i in 0..N {
        store
            .append_event_with(
                CH,
                human(),
                EventKind::Message,
                serde_json::json!({ "text": format!("m{i}") }),
                None,
                None,
                None,
                SealHooks::post(Some(&record_hook)),
            )
            .unwrap();
    }

    let identity = AgentIdentity::generate();
    let next = store
        .append_signed_event_with(
            CH,
            &identity,
            1,
            human(),
            EventKind::Message,
            serde_json::json!({ "text": "v2" }),
            None,
            SealHooks::default(),
        )
        .unwrap();

    let records = records.borrow();
    assert_eq!(records.len(), N);
    let last = records.last().unwrap();
    assert_eq!(last.last(), Some(&b'\n'), "record carries its terminator");

    let stripped = std::str::from_utf8(&last[..last.len() - 1]).unwrap();
    assert_ne!(stripped, "", "sanity: a non-empty line");
    assert_eq!(
        next.prev_hash.as_deref(),
        Some(line_hash(stripped).as_str()),
        "line_hash(record minus terminator) is the line the next v2 event chains"
    );
    assert_ne!(next.prev_hash.as_deref(), Some(GENESIS_PREV_HASH));
}
