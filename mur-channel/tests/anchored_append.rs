//! `ChannelService::append_and_anchor*` (#1770 part 2): the signed head of
//! #1868 and its dirty marker ride the store's seal hooks, under the lock.

use mur_channel::ChannelService;
use mur_channel::anchor::{self, AnchorLagError};
use mur_channel::head::chain_log;
use mur_channel::head_io::{head_path, read_head};
use mur_channel::store::AnchorMismatchKind;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

fn setup() -> (TempDir, ChannelService, String, AgentIdentity) {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap().id;
    (tmp, svc, ch, AgentIdentity::generate())
}

fn anchored(
    svc: &ChannelService,
    ch: &str,
    id: &AgentIdentity,
    key: Option<&str>,
) -> anyhow::Result<mur_common::channel::ChannelEvent> {
    svc.append_and_anchor(
        ch,
        id,
        1,
        ChannelActor::System,
        EventKind::Message,
        serde_json::json!({ "text": "x" }),
        key.map(str::to_string),
    )
}

fn plain(svc: &ChannelService, ch: &str, id: &AgentIdentity) {
    svc.append_signed(
        ch,
        id,
        1,
        ChannelActor::System,
        EventKind::Message,
        serde_json::json!({}),
        None,
    )
    .unwrap();
}

fn log_path(tmp: &TempDir, ch: &str) -> std::path::PathBuf {
    tmp.path().join("channels").join(ch).join("events.jsonl")
}

fn head(tmp: &TempDir, ch: &str) -> Option<mur_channel::head::Head> {
    read_head(&anchor::heads_dir(tmp.path()), ch).unwrap()
}

fn report(tmp: &TempDir, ch: &str, id: &AgentIdentity) -> mur_channel::store::DamageReport {
    mur_channel::ChannelStore::new(tmp.path())
        .load_events_with_damage(ch, &id.verifying_key_bytes(), false)
        .unwrap()
        .1
}

/// Make the head write fail (head path is a directory) without touching the
/// marker path, then return a guard-free path to undo it.
fn jam_head(tmp: &TempDir, ch: &str) -> std::path::PathBuf {
    let hp = head_path(&anchor::heads_dir(tmp.path()), ch).unwrap();
    let _ = std::fs::remove_file(&hp);
    std::fs::create_dir_all(&hp).unwrap();
    hp
}

#[test]
fn anchored_append_seals_a_verifying_head_over_the_whole_log() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    let ev = anchored(&svc, &ch, &id, None).unwrap();
    let h = head(&tmp, &ch).expect("head written");
    let state = chain_log(&std::fs::read(log_path(&tmp, &ch)).unwrap());
    assert!(h.verify(&id.verifying_key_bytes()));
    assert_eq!(h.seq, ev.seq);
    assert_eq!(h.lines, state.lines);
    assert_eq!(h.chain_hash, hex::encode(state.hash));
    assert_eq!(h.key_version, Some(1));
    assert_eq!(h.legacy_through, None, "head since the first line");
    assert!(!anchor::is_dirty(tmp.path(), &ch));
    assert!(report(&tmp, &ch, &id).is_clean());
}

#[test]
fn v1_anchored_append_seals_the_head() {
    let (tmp, svc, ch, id) = setup();
    let ev = svc
        .append_and_anchor_v1(
            &ch,
            &id,
            1,
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({}),
            None,
        )
        .unwrap();
    let h = head(&tmp, &ch).unwrap();
    assert_eq!((h.seq, h.lines), (ev.seq, 1));
    assert!(report(&tmp, &ch, &id).is_clean());
}

/// The first head on a channel with earlier, unanchored history records how
/// far back that history goes, signed, so it cannot be raised later.
#[test]
fn first_seal_over_legacy_lines_sets_legacy_through() {
    let (tmp, svc, ch, id) = setup();
    plain(&svc, &ch, &id);
    plain(&svc, &ch, &id);
    anchored(&svc, &ch, &id, None).unwrap();
    let h = head(&tmp, &ch).unwrap();
    assert_eq!(h.legacy_through, Some(1));
    // ...and it is carried forward, not recomputed.
    anchored(&svc, &ch, &id, None).unwrap();
    assert_eq!(head(&tmp, &ch).unwrap().legacy_through, Some(1));
}

/// Lines a sandboxed writer appended between two anchored appends are
/// covered by the next head: it chains the whole log, not one line.
#[test]
fn the_next_head_covers_lines_appended_past_the_previous_one() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    plain(&svc, &ch, &id);
    anchored(&svc, &ch, &id, None).unwrap();
    assert_eq!(head(&tmp, &ch).unwrap().lines, 3);
}

#[test]
fn plain_append_signed_never_touches_channel_heads() {
    let (tmp, svc, ch, id) = setup();
    plain(&svc, &ch, &id);
    assert!(!tmp.path().join(anchor::HEADS_DIR).exists());
}

/// The crash window #1861's post-only hook could not close: the line is
/// written and the head never advances. The marker went down BEFORE the
/// line, so the stale head is reported; the error is an `AnchorLagError`
/// reachable through anyhow's context chain.
#[test]
fn line_written_but_head_stuck_leaves_the_marker_and_a_downcastable_lag() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    let hp = jam_head(&tmp, &ch);

    let err = anchored(&svc, &ch, &id, None).unwrap_err();
    let lag = err
        .downcast_ref::<AnchorLagError>()
        .expect("AnchorLagError through context");
    assert_eq!(lag.event.seq, 1);
    let raw = std::fs::read_to_string(log_path(&tmp, &ch)).unwrap();
    assert!(
        raw.lines().last().unwrap().contains("\"seq\":1"),
        "line is durable"
    );
    assert!(anchor::is_dirty(tmp.path(), &ch));
    // Lift the fault so the verifier can read; the marker alone must flag it.
    std::fs::remove_dir(&hp).unwrap();
    let r = report(&tmp, &ch, &id);
    assert!(r.anchor_dirty);
    assert!(!r.is_clean());
}

/// From inside the sandbox `channel-heads/` is unwritable: `mark_dirty`
/// fails pre-write and the event is NOT written (never a half-anchored line).
#[test]
fn unwritable_heads_dir_aborts_before_the_line() {
    let (tmp, svc, ch, id) = setup();
    std::fs::write(tmp.path().join(anchor::HEADS_DIR), b"not a dir").unwrap();
    let before = std::fs::read_to_string(log_path(&tmp, &ch)).unwrap_or_default();
    let err = anchored(&svc, &ch, &id, None).unwrap_err();
    assert!(
        err.downcast_ref::<AnchorLagError>().is_none(),
        "nothing written, so no lag"
    );
    let after = std::fs::read_to_string(log_path(&tmp, &ch)).unwrap_or_default();
    assert_eq!(before, after);
}

/// A dedup hit on an older line is a no-op: the head already covers the log,
/// so it is neither moved back nor re-signed.
#[test]
fn dedup_hit_on_a_covered_line_is_a_no_op() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, Some("k0")).unwrap();
    anchored(&svc, &ch, &id, None).unwrap();
    let before = head(&tmp, &ch).unwrap();
    anchored(&svc, &ch, &id, Some("k0")).unwrap();
    assert_eq!(head(&tmp, &ch).unwrap(), before, "same head, same ts");
}

/// Aligned with #1868: a dedup hit still runs `post_write`. A crash after the
/// line and before the head leaves a marker and a lagging head; the retry
/// with the same key must repair both, without a duplicate line. (`pre_write`
/// is skipped on the hit — the marker is already there from the first try.)
#[test]
fn same_key_retry_after_lag_repairs_the_head_and_clears_the_marker() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    let hp = jam_head(&tmp, &ch);
    anchored(&svc, &ch, &id, Some("k")).unwrap_err();
    std::fs::remove_dir(&hp).unwrap();
    assert!(anchor::is_dirty(tmp.path(), &ch));

    let ev = anchored(&svc, &ch, &id, Some("k")).unwrap();
    let h = head(&tmp, &ch).unwrap();
    assert_eq!((h.seq, h.lines), (ev.seq, 2));
    assert!(!anchor::is_dirty(tmp.path(), &ch));
    let raw = std::fs::read_to_string(log_path(&tmp, &ch)).unwrap();
    assert_eq!(raw.lines().count(), 2, "no duplicate line");
    assert!(report(&tmp, &ch, &id).is_clean());
}

/// Monotonic: when the log no longer extends the signed head (truncated
/// below it), the next anchored append refuses to re-seal — that would
/// launder the truncation. The line lands, the lag is reported, the marker
/// stays, and the old head still convicts the log.
#[test]
fn a_log_truncated_below_the_head_is_never_resealed() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    anchored(&svc, &ch, &id, None).unwrap();
    let p = log_path(&tmp, &ch);
    let raw = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("{}\n", raw.lines().next().unwrap())).unwrap();
    let before = head(&tmp, &ch).unwrap();

    let err = anchored(&svc, &ch, &id, None).unwrap_err();
    assert!(err.downcast_ref::<AnchorLagError>().is_some());
    assert_eq!(head(&tmp, &ch).unwrap(), before, "head not moved");
    assert!(anchor::is_dirty(tmp.path(), &ch));
    let r = report(&tmp, &ch, &id);
    assert_eq!(
        r.anchor_mismatch.unwrap().kind,
        AnchorMismatchKind::Rewritten,
        "two lines again, but not the two the head covered"
    );
}

/// Truncation below the head is a mismatch even though the chain holds.
#[test]
fn truncating_the_covered_tail_is_reported() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    anchored(&svc, &ch, &id, None).unwrap();
    let p = log_path(&tmp, &ch);
    let raw = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, format!("{}\n", raw.lines().next().unwrap())).unwrap();
    let r = report(&tmp, &ch, &id);
    assert!(r.chain_breaks.is_empty());
    let m = r.anchor_mismatch.unwrap();
    assert_eq!((m.kind, m.log_lines), (AnchorMismatchKind::Truncated, 1));
}
