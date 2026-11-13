//! #2379: a `HitlRequest` that a human must answer is never written
//! unsigned. An unsigned request is one `hitl::authority` refuses to treat
//! as the router's question, so writing it only parks a card nobody can
//! approve.

use super::*;
use mur_common::channel::ChannelEvent;
use tempfile::TempDir;

fn refuse(require_sig: bool, awaits_answer: bool) -> Refuse {
    Refuse {
        require_sig,
        awaits_answer,
    }
}

fn request(home: &Path, ch: &str, r: Refuse) -> anyhow::Result<ChannelEvent> {
    let svc = ChannelService::open(home).unwrap();
    append_request_with(
        &svc,
        home,
        ch,
        ROUTER_AGENT,
        serde_json::json!({ "hitl_id": "h-1" }),
        r,
    )
}

fn channel(home: &Path) -> String {
    ChannelService::open(home)
        .unwrap()
        .create_for_workflow("g")
        .unwrap()
        .id
}

fn requests(home: &Path, ch: &str) -> usize {
    ChannelService::open(home)
        .unwrap()
        .load_events(ch)
        .unwrap()
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .count()
}

/// Make the router key unreadable for the duration of `f`, as the agent
/// sandbox does, and restore it so the TempDir can be cleaned up.
#[cfg(unix)]
fn with_unreadable_key<T>(home: &Path, f: impl FnOnce() -> T) -> T {
    use std::os::unix::fs::PermissionsExt;
    let key = mur_common::identity::private_key_dir(&home.join("agents").join(ROUTER_AGENT))
        .join("identity.key");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
    let out = f();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    out
}

#[cfg(unix)]
#[test]
fn an_awaited_request_is_refused_when_the_key_is_unreadable() {
    let _guard = super::tests::COUNTER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    plant_writer_identity(home);
    let ch = channel(home);
    let mark = refusal_count();

    // `require_sig` OFF: the refusal must not depend on it.
    let err = with_unreadable_key(home, || request(home, &ch, refuse(false, true)))
        .expect_err("an awaited request must not be written unsigned");

    assert!(
        err.downcast_ref::<UnanswerableRequest>().is_some(),
        "typed, so the gate's caller can name the remedy: {err:#}"
    );
    assert_eq!(requests(home, &ch), 0, "nothing may be parked");
    assert_eq!(refusals_since(mark), 1, "the refusal must be counted");
    let trail = std::fs::read_to_string(home.join("channels").join("write-refusals.jsonl"))
        .expect("the refusal must leave a trace");
    assert!(trail.contains("approval request"), "{trail}");
}

/// `--yes` / a pre-approved tier: the request is only an audit record of an
/// approval already given, so the old warn-and-write path still holds.
#[cfg(unix)]
#[test]
fn an_audit_only_request_still_writes_when_the_key_is_unreadable() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    plant_writer_identity(home);
    let ch = channel(home);

    let ev = with_unreadable_key(home, || request(home, &ch, refuse(false, false)))
        .expect("an audit-only request keeps the old behaviour");
    assert!(ev.sig.is_none());
}

/// A keyless home (bootstrap, test fixtures) has no router to answer as at
/// all, so an absent key stays governed by `require_sig` alone.
#[test]
fn an_awaited_request_in_a_keyless_home_is_unchanged() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let ch = channel(home);

    let ev = request(home, &ch, refuse(false, true)).expect("bootstrap path must not fail");
    assert!(ev.sig.is_none());
}

#[test]
fn an_awaited_request_with_a_readable_key_is_signed() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    plant_writer_identity(home);
    let ch = channel(home);

    let ev = request(home, &ch, refuse(false, true)).unwrap();
    assert!(ev.sig.is_some(), "the normal path is untouched");
}
