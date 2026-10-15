//! Gaps in `append_and_anchor` found by probing `main` (#1770): each test
//! fails without the matching guard in `service/anchored.rs`.

use mur_channel::ChannelService;
use mur_channel::anchor;
use mur_channel::head::{Head, HeadFields, chain_log};
use mur_channel::head_io::{head_path, read_head, write_head};
use mur_common::channel::{ChannelActor, ChannelState, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

fn setup() -> (TempDir, ChannelService, String, AgentIdentity) {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap().id;
    (tmp, svc, ch, AgentIdentity::generate())
}

fn anchored(svc: &ChannelService, ch: &str, id: &AgentIdentity) -> anyhow::Result<()> {
    svc.append_and_anchor(
        ch,
        id,
        1,
        ChannelActor::System,
        EventKind::Message,
        serde_json::json!({ "text": "x" }),
        None,
    )
    .map(|_| ())
}

fn log_path(tmp: &TempDir, ch: &str) -> std::path::PathBuf {
    tmp.path().join("channels").join(ch).join("events.jsonl")
}

fn head(tmp: &TempDir, ch: &str) -> Option<Head> {
    read_head(&anchor::heads_dir(tmp.path()), ch).unwrap()
}

/// A head chaining the current log, sealed by `signer` for `channel_id`.
fn head_over_log(tmp: &TempDir, ch: &str, channel_id: &str, signer: &AgentIdentity) -> Head {
    let st = chain_log(&std::fs::read(log_path(tmp, ch)).unwrap());
    Head::seal(
        signer,
        HeadFields {
            channel_id: channel_id.to_owned(),
            seq: 1,
            lines: st.lines,
            chain_hash: hex::encode(st.hash),
            key_version: Some(1),
            legacy_through: None,
        },
        chrono::Utc::now(),
    )
}

#[test]
fn a_previous_head_signed_by_another_key_is_refused_not_replaced() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id).unwrap();
    let forged = head_over_log(&tmp, &ch, &ch, &AgentIdentity::generate());
    write_head(&anchor::heads_dir(tmp.path()), &forged).unwrap();

    assert!(
        anchored(&svc, &ch, &id).is_err(),
        "forged head was accepted"
    );
    assert_eq!(
        head(&tmp, &ch),
        Some(forged),
        "the forged head was overwritten"
    );
}

#[test]
fn a_previous_head_naming_another_channel_is_refused() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id).unwrap();
    let wrong = head_over_log(&tmp, &ch, "some-other-channel", &id);
    // `write_head` derives the path from `channel_id`; place the file by hand.
    let p = head_path(&anchor::heads_dir(tmp.path()), &ch).unwrap();
    std::fs::write(&p, serde_json::to_vec(&wrong).unwrap()).unwrap();

    assert!(
        anchored(&svc, &ch, &id).is_err(),
        "wrong-channel head accepted"
    );
}

#[test]
fn first_seal_over_only_unparseable_lines_is_refused() {
    let (tmp, svc, ch, id) = setup();
    let lp = log_path(&tmp, &ch);
    std::fs::create_dir_all(lp.parent().unwrap()).unwrap();
    std::fs::write(&lp, b"not json at all\nstill not json\n").unwrap();

    assert!(anchored(&svc, &ch, &id).is_err());
    assert!(
        head(&tmp, &ch).is_none(),
        "a head was sealed with legacy_through None"
    );
}

#[test]
fn an_anchored_transition_keeps_the_head_on_the_log() {
    let (tmp, svc, ch, id) = setup();
    anchored(&svc, &ch, &id).unwrap();
    svc.transition_and_anchor(
        &ch,
        ChannelState::Completed,
        ChannelActor::System,
        None,
        &id,
        1,
    )
    .unwrap();

    let lines = chain_log(&std::fs::read(log_path(&tmp, &ch)).unwrap()).lines;
    assert_eq!(head(&tmp, &ch).unwrap().lines, lines, "head lags the log");
    assert!(!anchor::is_dirty(tmp.path(), &ch));
}
