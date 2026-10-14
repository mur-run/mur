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
