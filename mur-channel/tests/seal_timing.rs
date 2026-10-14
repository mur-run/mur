//! Cost of the full 7.3.1 seal flow on a large legacy log (spec 12 item 12).
//!
//! Measurement, not a gate: `#[ignore]`d, prints timings. Run with
//! `cargo test -p mur-channel --release --test seal_timing -- --ignored --nocapture`.
//! Sizes come from `MUR_SEAL_TIMING_LINES` (comma separated line counts).

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use mur_channel::ChannelStore;
use mur_channel::seal::HeadSealer;
use mur_common::channel::{Channel, ChannelActor, ChannelEvent, ChannelState, EventKind, Goal};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

const CH: &str = "timing-ch";
const DEFAULT_SIZES: &str = "10000,100000,500000";
/// Payload bytes per legacy line, so the log size is realistic, not tiny.
const PAYLOAD_BYTES: usize = 200;

fn human() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

fn append(store: &ChannelStore, text: &str, seal: Option<&mur_channel::store::SealHook<'_>>) {
    store
        .append_event_with(
            CH,
            human(),
            EventKind::Message,
            serde_json::json!({ "text": text }),
            None,
            None,
            None,
            seal,
        )
        .unwrap();
}

/// A channel whose log holds `lines` legacy lines, written directly (the
/// per-append path takes a lock and is far slower than the log it builds).
fn legacy_channel(lines: usize) -> (TempDir, ChannelStore, u64) {
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
    append(&store, &"x".repeat(PAYLOAD_BYTES), None);
    let path = store.events_path(CH);
    let first = std::fs::read_to_string(&path).unwrap();
    let template: ChannelEvent = serde_json::from_str(first.lines().next().unwrap()).unwrap();

    let mut out = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap(),
    );
    for i in 0..lines {
        let mut ev = template.clone();
        ev.seq = i as u64;
        serde_json::to_writer(&mut out, &ev).unwrap();
        out.write_all(b"\n").unwrap();
    }
    out.flush().unwrap();
    let bytes = std::fs::metadata(&path).unwrap().len();
    (tmp, store, bytes)
}

#[test]
#[ignore = "measurement; run explicitly with --release --ignored --nocapture"]
fn seal_cost_on_a_large_legacy_log() {
    let sizes = std::env::var("MUR_SEAL_TIMING_LINES").unwrap_or_else(|_| DEFAULT_SIZES.into());
    println!("lines      log_MB   plain_ms   first_seal_ms   next_seal_ms");
    for n in sizes.split(',').map(|s| s.trim().parse::<usize>().unwrap()) {
        let (tmp, store, bytes) = legacy_channel(n);
        // Baseline: an unsealed append on the same log (seq scan, dedup, lock).
        let t = Instant::now();
        append(&store, "plain", None);
        let plain = t.elapsed();

        let id = Arc::new(AgentIdentity::generate());
        let sealer = HeadSealer::new(tmp.path(), id, 1);
        let hook = move |i: &mur_channel::store::SealInput<'_>| sealer.seal(i);

        let t = Instant::now();
        append(&store, "router first", Some(&hook));
        let first = t.elapsed();

        let t = Instant::now();
        append(&store, "router second", Some(&hook));
        let next = t.elapsed();

        println!(
            "{n:<10} {:<8.1} {:<10.1} {:<15.1} {:.1}",
            bytes as f64 / 1e6,
            plain.as_secs_f64() * 1e3,
            first.as_secs_f64() * 1e3,
            next.as_secs_f64() * 1e3
        );
    }
}
