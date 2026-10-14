//! `ChannelService::append_and_anchor*` (#1770): the chain-head anchor and
//! its dirty marker ride the store's seal hooks, under the channel lock.

use mur_channel::ChannelService;
use mur_channel::anchor::{self, AnchorLagError};
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

fn last_line(tmp: &TempDir, ch: &str) -> String {
    let raw =
        std::fs::read_to_string(tmp.path().join("channels").join(ch).join("events.jsonl")).unwrap();
    raw.lines().last().unwrap().to_string()
}

fn report(tmp: &TempDir, svc_ch: &str, id: &AgentIdentity) -> mur_channel::store::DamageReport {
    mur_channel::ChannelStore::new(tmp.path())
        .load_events_with_damage(svc_ch, &id.verifying_key_bytes(), false)
        .unwrap()
        .1
}

#[test]
fn anchored_append_points_the_anchor_at_the_line_and_clears_the_marker() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    let ev = anchored(&svc, &ch, &id, None).unwrap();
    let a = anchor::read_anchor(tmp.path(), &ch).unwrap().unwrap();
    assert_eq!(a.seq, ev.seq);
    assert_eq!(a.hash, mur_channel::store::line_hash(&last_line(&tmp, &ch)));
    assert!(!anchor::is_dirty(tmp.path(), &ch));
    assert!(report(&tmp, &ch, &id).is_clean());
}

#[test]
fn v1_anchored_append_points_the_anchor_at_the_line() {
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
    let a = anchor::read_anchor(tmp.path(), &ch).unwrap().unwrap();
    assert_eq!(a.seq, ev.seq);
    assert_eq!(a.hash, mur_channel::store::line_hash(&last_line(&tmp, &ch)));
}

#[test]
fn plain_append_signed_never_touches_channel_heads() {
    let (tmp, svc, ch, id) = setup();
    svc.append_signed(
        &ch,
        &id,
        1,
        ChannelActor::System,
        EventKind::Message,
        serde_json::json!({}),
        None,
    )
    .unwrap();
    assert!(!tmp.path().join(anchor::ANCHOR_DIR).exists());
}

/// The crash window #1861's post-only hook could not close: the line is
/// written and the anchor never advances. Simulated by making the anchor
/// write fail (anchor path is a directory). The marker went down BEFORE the
/// line, so the stale anchor is reported; the error is an `AnchorLagError`
/// reachable through anyhow's context chain.
#[test]
fn line_written_but_anchor_stuck_leaves_the_marker_and_a_downcastable_lag() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    std::fs::remove_file(anchor::anchor_path(tmp.path(), &ch)).unwrap();
    std::fs::create_dir(anchor::anchor_path(tmp.path(), &ch)).unwrap();

    let err = anchored(&svc, &ch, &id, None).unwrap_err();
    let lag = err
        .downcast_ref::<AnchorLagError>()
        .expect("AnchorLagError through context");
    assert_eq!(lag.event.seq, 1);
    assert!(
        last_line(&tmp, &ch).contains("\"seq\":1"),
        "line is durable"
    );
    assert!(anchor::is_dirty(tmp.path(), &ch));
    // Lift the fault so the verifier can read; the marker alone must flag it.
    std::fs::remove_dir(anchor::anchor_path(tmp.path(), &ch)).unwrap();
    assert!(report(&tmp, &ch, &id).anchor_dirty);
    assert!(!report(&tmp, &ch, &id).is_clean());
}

/// From inside the sandbox `channel-heads/` is unwritable: `mark_dirty`
/// fails pre-write and the event is NOT written (never a half-anchored line).
#[test]
fn unwritable_heads_dir_aborts_before_the_line() {
    let (tmp, svc, ch, id) = setup();
    std::fs::write(tmp.path().join(anchor::ANCHOR_DIR), b"not a dir").unwrap();
    let before =
        std::fs::read_to_string(tmp.path().join("channels").join(&ch).join("events.jsonl"))
            .unwrap_or_default();
    let err = anchored(&svc, &ch, &id, None).unwrap_err();
    assert!(
        err.downcast_ref::<AnchorLagError>().is_none(),
        "nothing written, so no lag"
    );
    let after = std::fs::read_to_string(tmp.path().join("channels").join(&ch).join("events.jsonl"))
        .unwrap_or_default();
    assert_eq!(before, after);
}

/// A dedup retry hands `post_write` an EARLIER line. The anchor must never
/// move backwards — that would make truncation below the real head pass.
#[test]
fn dedup_retry_never_moves_the_anchor_backwards() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, Some("k0")).unwrap();
    let head = anchored(&svc, &ch, &id, None).unwrap();
    anchored(&svc, &ch, &id, Some("k0")).unwrap();
    assert_eq!(
        anchor::read_anchor(tmp.path(), &ch).unwrap().unwrap().seq,
        head.seq
    );
}

/// A retry with the same key after an `AnchorLagError` repairs the anchor
/// without a duplicate line, and clears the marker.
#[test]
fn same_key_retry_after_lag_repairs_the_anchor() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    let ap = anchor::anchor_path(tmp.path(), &ch);
    std::fs::remove_file(&ap).unwrap();
    std::fs::create_dir(&ap).unwrap();
    anchored(&svc, &ch, &id, Some("k")).unwrap_err();
    std::fs::remove_dir(&ap).unwrap();

    let ev = anchored(&svc, &ch, &id, Some("k")).unwrap();
    assert_eq!(
        anchor::read_anchor(tmp.path(), &ch).unwrap().unwrap().seq,
        ev.seq
    );
    assert!(!anchor::is_dirty(tmp.path(), &ch));
    let raw = std::fs::read_to_string(tmp.path().join("channels").join(&ch).join("events.jsonl"))
        .unwrap();
    assert_eq!(raw.lines().count(), 2, "no duplicate line");
}

/// Truncation below the anchor is a mismatch even though the chain holds.
#[test]
fn truncating_the_anchored_tail_is_reported() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id, None).unwrap();
    anchored(&svc, &ch, &id, None).unwrap();
    let p = tmp.path().join("channels").join(&ch).join("events.jsonl");
    let raw = std::fs::read_to_string(&p).unwrap();
    let first = raw.lines().next().unwrap();
    std::fs::write(&p, format!("{first}\n")).unwrap();
    let r = report(&tmp, &ch, &id);
    assert!(r.chain_breaks.is_empty());
    assert_eq!(r.anchor_mismatch.unwrap().found_seq, Some(0));
}
