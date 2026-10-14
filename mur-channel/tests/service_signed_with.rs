//! `ChannelService::append_signed_with` lends a per-call seal hook to the
//! signed append, so a caller that builds its hook from the writer's key (the
//! key is only known at call time) can still seal under the same lock
//! (channel-log-integrity spec 7.3.1, #1770).

use std::sync::{Arc, Mutex};

use mur_channel::ChannelService;
use mur_channel::service::SharedSealHook;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

fn human() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

fn payload() -> serde_json::Value {
    serde_json::json!({ "text": "signed" })
}

#[test]
fn per_call_hook_runs_on_a_service_without_one() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = svc.create_for_agent("seal-agent").unwrap().id;
    let identity = AgentIdentity::generate();

    let seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let hook = move |input: &mur_channel::store::SealInput<'_>| {
        sink.lock().unwrap().push(input.event.seq);
        Ok(())
    };

    let ev = svc
        .append_signed_with(
            &id,
            &identity,
            1,
            human(),
            EventKind::Message,
            payload(),
            None,
            Some(&hook),
        )
        .unwrap();

    assert_eq!(
        *seen.lock().unwrap(),
        vec![ev.seq],
        "hook ran once, on this event"
    );
}

#[test]
fn none_behaves_like_append_signed() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = svc.create_for_agent("seal-agent").unwrap().id;
    let identity = AgentIdentity::generate();

    let ev = svc
        .append_signed_with(
            &id,
            &identity,
            1,
            human(),
            EventKind::Message,
            payload(),
            None,
            None,
        )
        .unwrap();

    assert!(ev.sig.is_some(), "still signed");
    let row = svc
        .list(10)
        .unwrap()
        .into_iter()
        .find(|r| r.id == id)
        .expect("channel is in the read model");
    assert_eq!(row.last_seq, ev.seq as i64, "read model refreshed");
}

#[test]
fn per_call_hook_replaces_the_service_hook() {
    // One seal per event: the service-level hook must not also fire.
    let tmp = TempDir::new().unwrap();
    let service_calls = Arc::new(Mutex::new(0u32));
    let c = service_calls.clone();
    let service_hook: SharedSealHook = Arc::new(move |_| {
        *c.lock().unwrap() += 1;
        Ok(())
    });
    let svc = ChannelService::open(tmp.path())
        .unwrap()
        .with_seal(service_hook);
    let id = svc.create_for_agent("seal-agent").unwrap().id;
    let before = *service_calls.lock().unwrap();
    let identity = AgentIdentity::generate();

    let per_call = Arc::new(Mutex::new(0u32));
    let p = per_call.clone();
    let hook = move |_: &mur_channel::store::SealInput<'_>| {
        *p.lock().unwrap() += 1;
        Ok(())
    };
    svc.append_signed_with(
        &id,
        &identity,
        1,
        human(),
        EventKind::Message,
        payload(),
        None,
        Some(&hook),
    )
    .unwrap();

    assert_eq!(*per_call.lock().unwrap(), 1);
    assert_eq!(
        *service_calls.lock().unwrap(),
        before,
        "service hook skipped"
    );
}

#[test]
fn a_failing_per_call_hook_fails_the_write_but_leaves_the_line() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = svc.create_for_agent("seal-agent").unwrap().id;
    let identity = AgentIdentity::generate();
    let before = svc.load_events(&id).unwrap().len();

    let hook = |_: &mur_channel::store::SealInput<'_>| anyhow::bail!("seal down");
    let r = svc.append_signed_with(
        &id,
        &identity,
        1,
        human(),
        EventKind::Message,
        payload(),
        None,
        Some(&hook),
    );

    assert!(r.is_err(), "B1: a failed seal must surface as Err");
    assert_eq!(svc.load_events(&id).unwrap().len(), before + 1);
}
