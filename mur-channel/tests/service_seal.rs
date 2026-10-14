//! `ChannelService::with_seal` wires the store's sealing hook into every
//! service write path (channel-log-integrity spec 7.3.1, commit 2). The hook
//! implementation (head files, chain hashes) is a later slice; these tests use
//! a recording fake and only prove the plumbing.

use std::sync::{Arc, Mutex};

use mur_channel::ChannelService;
use mur_channel::service::SharedSealHook;
use mur_common::channel::{ChannelActor, ChannelState, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

type Seen = Arc<Mutex<Vec<(String, u64, EventKind)>>>;

fn recording_hook() -> (SharedSealHook, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let hook: SharedSealHook = Arc::new(move |input| {
        sink.lock().unwrap().push((
            input.channel_id.to_string(),
            input.event.seq,
            input.event.kind,
        ));
        Ok(())
    });
    (hook, seen)
}

fn human() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

fn exercise_all_paths(svc: &ChannelService) -> String {
    let ch = svc.create_for_agent("seal-agent").unwrap();
    let id = ch.id.clone();
    let identity = AgentIdentity::generate();
    svc.append_message(&id, human(), EventKind::Message, "hi", None)
        .unwrap();
    svc.append(
        &id,
        ChannelActor::System,
        EventKind::Delegation,
        serde_json::json!({ "n": 1 }),
        None,
    )
    .unwrap();
    svc.append_signed(
        &id,
        &identity,
        1,
        human(),
        EventKind::Message,
        serde_json::json!({ "text": "signed" }),
        None,
    )
    .unwrap();
    svc.transition_signed(
        &id,
        ChannelState::Completed,
        ChannelActor::System,
        None,
        Some((&identity, 1)),
    )
    .unwrap();
    id
}

#[test]
fn every_service_write_path_runs_the_seal_hook() {
    let tmp = TempDir::new().unwrap();
    let (hook, seen) = recording_hook();
    let svc = ChannelService::open(tmp.path()).unwrap().with_seal(hook);
    let id = exercise_all_paths(&svc);

    let seen = seen.lock().unwrap();
    // create_for_agent may write events of its own; the four explicit writes
    // must be the last four, in order, each sealed exactly once.
    let tail: Vec<_> = seen.iter().rev().take(4).rev().collect();
    assert_eq!(tail.len(), 4, "seen: {seen:?}");
    assert!(tail.iter().all(|(c, _, _)| c == &id));
    let kinds: Vec<_> = tail.iter().map(|(_, _, k)| *k).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::Message,
            EventKind::Delegation,
            EventKind::Message,
            EventKind::StateChange
        ]
    );
    let seqs: Vec<_> = tail.iter().map(|(_, s, _)| *s).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "seqs: {seqs:?}");
}

#[test]
fn default_service_has_no_hook_and_still_writes() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = exercise_all_paths(&svc);
    let events = svc.load_events(&id).unwrap();
    assert!(events.len() >= 4, "events: {}", events.len());
}

#[test]
fn a_failing_hook_fails_the_write_but_leaves_the_line() {
    let tmp = TempDir::new().unwrap();
    let hook: SharedSealHook = Arc::new(|_| anyhow::bail!("seal down"));
    let svc = ChannelService::open(tmp.path()).unwrap().with_seal(hook);
    let ch = svc.create_for_agent("seal-agent").unwrap_or_else(|_| {
        // create_for_agent may itself write through the hook; fall back to a
        // hook-free service to get a channel id.
        let plain = ChannelService::open(tmp.path()).unwrap();
        plain.create_for_agent("seal-agent").unwrap()
    });
    let before = svc.load_events(&ch.id).unwrap().len();
    let r = svc.append_message(&ch.id, human(), EventKind::Message, "x", None);
    assert!(r.is_err(), "B1: a failed seal must surface as Err");
    assert_eq!(svc.load_events(&ch.id).unwrap().len(), before + 1);
}

// --- append_presigned (commit 3) ---------------------------------------------
// The Commander governance receipt arrives already signed (the Commander's key,
// not ours) and is verified by the caller. The service only persists + seals it.

#[test]
fn append_presigned_persists_sig_verbatim_and_runs_the_seal_hook() {
    let tmp = TempDir::new().unwrap();
    let (hook, seen) = recording_hook();
    let svc = ChannelService::open(tmp.path()).unwrap().with_seal(hook);
    let id = svc.create_for_agent("seal-agent").unwrap().id;
    let identity = AgentIdentity::generate();
    let payload = serde_json::json!({ "directive": "pause" });
    let sig = mur_channel::sign::sign_event(
        &identity,
        &id,
        &ChannelActor::System,
        EventKind::Message,
        &payload,
        Some("nonce-1"),
    );

    let ev = svc
        .append_presigned(
            &id,
            ChannelActor::System,
            EventKind::Message,
            payload,
            Some("nonce-1".into()),
            sig.clone(),
            Some(7),
        )
        .unwrap();

    assert_eq!(ev.sig.as_deref(), Some(sig.as_str()));
    assert_eq!(ev.key_version, Some(7));
    let on_disk = svc.load_events(&id).unwrap();
    assert_eq!(on_disk.last().unwrap().sig.as_deref(), Some(sig.as_str()));
    assert_eq!(seen.lock().unwrap().len(), 1, "hook ran exactly once");
}

#[test]
fn append_presigned_refreshes_the_read_model() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = svc.create_for_agent("seal-agent").unwrap().id;

    let ev = svc
        .append_presigned(
            &id,
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({ "x": 1 }),
            None,
            "z-opaque".into(),
            Some(1),
        )
        .unwrap();

    let row = svc
        .list(10)
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .expect("channel is in the read model");
    assert_eq!(row.last_seq, ev.seq as i64, "index saw the presigned event");
}

#[test]
fn append_presigned_does_not_verify_the_signature() {
    // Verification stays with the caller (governance checks the Commander's
    // key). The service must not second-guess a sig it cannot attribute.
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = svc.create_for_agent("seal-agent").unwrap().id;
    let ev = svc
        .append_presigned(
            &id,
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({}),
            None,
            "not-a-real-signature".into(),
            None,
        )
        .unwrap();
    assert_eq!(ev.sig.as_deref(), Some("not-a-real-signature"));
}
