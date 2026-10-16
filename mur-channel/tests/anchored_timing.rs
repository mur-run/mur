//! Cost of `ChannelService::append_and_anchor` on a large legacy log (#1930,
//! parent #1770).
//!
//! The first anchor on a channel seals a head over the WHOLE existing log, so
//! its cost scales with the log. Whether a later anchor is cheaper is exactly
//! what this measures; nothing here assumes it is. It reports plain append,
//! the first anchor, and the min / median / max of [`NEXT_ANCHORS`] later
//! anchors, so a regression in `advance` shows up as a distribution rather
//! than a single noisy sample.
//!
//! Measurement, not a gate: `#[ignore]`d, prints timings. Run with
//! `cargo test -p mur-channel --release --test anchored_timing -- --ignored --nocapture`.
//! Sizes come from `MUR_ANCHOR_TIMING_LINES` (comma separated line counts).

use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

use mur_channel::store::SealHooks;
use mur_channel::{ChannelService, ChannelStore};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

const DEFAULT_SIZES: &str = "10000,100000";
const SIZES_ENV: &str = "MUR_ANCHOR_TIMING_LINES";
/// Payload bytes per legacy line, so the log size is realistic, not tiny.
const PAYLOAD_BYTES: usize = 200;
const KV: u32 = 1;

fn actor() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

/// A channel whose log holds `lines` unsigned legacy lines, written directly
/// (the per-append path takes a lock and is far slower than the log it
/// builds). Returns the log size in bytes.
fn legacy_channel(home: &Path, svc: &ChannelService, lines: usize) -> (String, u64) {
    let id = svc.create_for_workflow("timing").unwrap().id;
    let store = ChannelStore::new(home);
    store
        .append_event_with(
            &id,
            actor(),
            EventKind::Message,
            serde_json::json!({ "text": "x".repeat(PAYLOAD_BYTES) }),
            None,
            None,
            None,
            SealHooks::default(),
        )
        .unwrap();
    let path = store.events_path(&id);
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
    (id, bytes)
}

/// Later anchors timed per size; the median is the figure to compare.
const NEXT_ANCHORS: usize = 9;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

#[test]
#[ignore = "measurement; run explicitly with --release --ignored --nocapture"]
fn anchor_cost_on_a_large_legacy_log() {
    let sizes = std::env::var(SIZES_ENV).unwrap_or_else(|_| DEFAULT_SIZES.into());
    println!(
        "lines      log_MB   plain_ms   first_anchor_ms   next_anchor_ms (min / median / max of {NEXT_ANCHORS})"
    );
    for n in sizes.split(',').map(|s| s.trim().parse::<usize>().unwrap()) {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let (ch, bytes) = legacy_channel(tmp.path(), &svc, n);
        let id = AgentIdentity::generate();
        let payload = || serde_json::json!({ "text": "t" });

        // Baseline: an unanchored signed append on the same log (seq scan,
        // dedup, lock).
        let t = Instant::now();
        svc.append_signed(&ch, &id, KV, actor(), EventKind::Message, payload(), None)
            .unwrap();
        let plain = ms(t);

        let anchor = |idem: &str| {
            let t = Instant::now();
            svc.append_and_anchor(
                &ch,
                &id,
                KV,
                Path::new(""),
                actor(),
                EventKind::Message,
                payload(),
                Some(idem.into()),
            )
            .unwrap();
            ms(t)
        };
        let first = anchor("first");
        let mut next: Vec<f64> = (0..NEXT_ANCHORS)
            .map(|i| anchor(&format!("next-{i}")))
            .collect();
        next.sort_by(f64::total_cmp);

        println!(
            "{n:<10} {:<8.1} {:<10.1} {:<17.1} {:.1} / {:.1} / {:.1}",
            bytes as f64 / 1e6,
            plain,
            first,
            next[0],
            next[NEXT_ANCHORS / 2],
            next[NEXT_ANCHORS - 1]
        );
    }
}
