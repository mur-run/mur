//! `mur channel delete` against a real temp `~/.mur`: channels go through the
//! production writer so heads are anchored, and agent histories sit where the
//! runtime writes them.

use super::*;
use crate::channel_writer::{Authorship, append_as_writer, plant_writer_identity};
use mur_channel::service::ChannelOwner;
use mur_common::channel::EventKind;
use mur_common::hitl::{HitlRequest, RiskTier};
use tempfile::TempDir;

fn home() -> TempDir {
    let tmp = TempDir::new().unwrap();
    plant_writer_identity(tmp.path());
    tmp
}

fn svc(home: &Path) -> ChannelService {
    ChannelService::open(home).unwrap()
}

/// An agent directory `agent_dir_names` recognises.
fn agent(home: &Path, name: &str) -> PathBuf {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(dir.join("conversations")).unwrap();
    std::fs::write(dir.join("profile.yaml"), format!("name: {name}\n")).unwrap();
    dir.join("conversations")
}

fn message(home: &Path, ch: &str, text: &str) {
    append_as_writer(
        &svc(home),
        home,
        ch,
        Authorship::system(),
        EventKind::Message,
        serde_json::json!({ "text": text }),
        None,
    )
    .unwrap();
}

/// A conversation with one message and a history file in `aura`'s home.
fn conversation(home: &Path) -> (String, PathBuf) {
    let ch = svc(home)
        .create_for_agent("aura", ChannelOwner::Legacy)
        .unwrap();
    message(home, &ch.id, "a secret pasted by mistake");
    let hist = agent(home, "aura").join(format!("{}.json", ch.id));
    std::fs::write(&hist, b"[]").unwrap();
    (ch.id, hist)
}

fn index_has(home: &Path, id: &str) -> bool {
    svc(home)
        .index()
        .list(1000)
        .unwrap()
        .iter()
        .any(|r| r.id == id)
}

fn head(home: &Path, id: &str) -> PathBuf {
    mur_channel::head_io::head_path(&mur_channel::anchor::heads_dir(home), id).unwrap()
}

/// A clock past the recent-activity window, so fixtures written a moment
/// ago read as idle.
fn idle() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + RECENT_ACTIVITY_WINDOW + chrono::TimeDelta::seconds(1)
}

/// `delete` as an operator runs it once the conversation has gone quiet.
fn delete_at(home: &Path, handle: &str, force: bool, confirm: Confirm<'_>) -> Result<Outcome> {
    delete(home, handle, force, idle(), confirm)
}

fn answer(text: &str) -> std::io::Cursor<Vec<u8>> {
    std::io::Cursor::new(text.as_bytes().to_vec())
}

#[test]
fn deleting_a_conversation_removes_all_five_places() {
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    assert!(head(h, &id).exists(), "fixture must anchor a head");
    assert!(index_has(h, &id));

    let out = delete_at(h, &id, false, Confirm::Yes).unwrap();

    assert_eq!(
        out,
        Outcome::Deleted {
            id: id.clone(),
            histories: vec![hist.clone()]
        }
    );
    assert!(!hist.exists(), "agent history left behind");
    assert!(!svc(h).store().has_manifest(&id), "store dir left behind");
    assert!(!index_has(h, &id), "index row left behind");
    assert!(!head(h, &id).exists(), "signed head left behind");
    assert!(!mur_channel::anchor::dirty_path(h, &id).unwrap().exists());
    let hits = svc(h).index().search_bodies("secret", 10).unwrap();
    assert!(hits.is_empty(), "message text still searchable: {hits:?}");
}

#[test]
fn a_handle_prefix_names_the_conversation() {
    let tmp = home();
    let h = tmp.path();
    let (id, _) = conversation(h);
    let handle = svc(h).index().handles().unwrap().of(&id).unwrap();
    delete_at(h, &handle, false, Confirm::Yes).unwrap();
    assert!(!index_has(h, &id));
}

#[test]
fn an_ambiguous_prefix_deletes_nothing() {
    let tmp = home();
    let h = tmp.path();
    let (a, ha) = conversation(h);
    let (b, hb) = conversation(h);
    // Same millisecond window: UUIDv7 ids share their leading hex.
    let shared: String = handle_key(&a)
        .unwrap()
        .chars()
        .zip(handle_key(&b).unwrap().chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x)
        .collect();
    assert!(shared.len() >= MIN_PREFIX, "fixture ids too far apart");

    let err = delete_at(h, &shared, false, Confirm::Yes).unwrap_err();

    assert!(err.to_string().contains("matches 2 channels"), "{err}");
    assert!(index_has(h, &a) && index_has(h, &b));
    assert!(ha.exists() && hb.exists());
}

#[test]
fn a_prefix_below_the_floor_is_refused() {
    let tmp = home();
    let err = delete_at(tmp.path(), "01a", false, Confirm::Yes).unwrap_err();
    assert!(err.to_string().contains("too short"), "{err}");
}

#[test]
fn a_run_channel_is_refused() {
    let tmp = home();
    let h = tmp.path();
    let ch = svc(h)
        .create_for_workflow("nightly", ChannelOwner::Legacy)
        .unwrap();
    message(h, &ch.id, "step done");

    let err = delete_at(h, &ch.id, true, Confirm::Yes).unwrap_err();

    assert!(err.to_string().contains("not a conversation"), "{err}");
    assert!(index_has(h, &ch.id));
    assert!(svc(h).store().has_manifest(&ch.id));
}

#[test]
fn a_pending_approval_refuses_without_force() {
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    let req = HitlRequest {
        hitl_id: "h1".into(),
        action_hash: "deadbeef".into(),
        tier: RiskTier::Write,
        tool_name: "fs_write".into(),
        tool_input: serde_json::json!({}),
        step_or_call_id: "step-1".into(),
        agent_id: "aura".into(),
        timeout_ms: 60_000,
        summary: "write config.yaml".into(),
        issued_at: Some(chrono::Utc::now()),
    };
    append_as_writer(
        &svc(h),
        h,
        &id,
        Authorship::system(),
        EventKind::HitlRequest,
        serde_json::to_value(&req).unwrap(),
        None,
    )
    .unwrap();

    let err = delete_at(h, &id, false, Confirm::Yes).unwrap_err();
    assert!(err.to_string().contains("pending approval"), "{err}");
    assert!(hist.exists() && index_has(h, &id));

    delete_at(h, &id, true, Confirm::Yes).unwrap();
    assert!(!hist.exists() && !index_has(h, &id));
}

#[test]
fn answering_no_keeps_everything() {
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    let mut input = answer("n\n");
    let out = delete_at(
        h,
        &id,
        false,
        Confirm::Ask {
            input: &mut input,
            interactive: true,
        },
    )
    .unwrap();
    assert_eq!(out, Outcome::Declined);
    assert!(hist.exists() && index_has(h, &id) && head(h, &id).exists());
}

#[test]
fn no_terminal_and_no_yes_is_refused() {
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    let mut input = answer("y\n");
    let err = delete_at(
        h,
        &id,
        false,
        Confirm::Ask {
            input: &mut input,
            interactive: false,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("pass --yes"), "{err}");
    assert!(hist.exists() && index_has(h, &id));
}

#[test]
fn a_departed_agent_copy_is_removed_too() {
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    let other = agent(h, "buildy").join(format!("{id}.json"));
    std::fs::write(&other, b"[]").unwrap();

    let Outcome::Deleted { histories, .. } = delete_at(h, &id, false, Confirm::Yes).unwrap() else {
        panic!("not deleted");
    };

    assert_eq!(histories.len(), 2);
    assert!(!hist.exists() && !other.exists());
}

#[test]
fn a_crash_after_the_store_is_gone_leaves_log_missing_not_no_head() {
    // `delete_channel`'s order, pinned: with only the store and index gone
    // (a crash before the head step), the head is an orphan, never absent
    // beside a live log.
    let tmp = home();
    let h = tmp.path();
    let (id, _) = conversation(h);
    svc(h).store().delete(&id).unwrap();
    svc(h).index().remove(&id).unwrap();
    assert!(head(h, &id).exists(), "head must outlive the log");
    assert!(!svc(h).store().has_manifest(&id));
    // Deleting again finishes the job.
    svc(h).delete_channel(&id).unwrap();
    assert!(!head(h, &id).exists());
}

#[test]
fn a_recently_active_conversation_refuses_without_force() {
    // A turn may still be running: its history file would be written back
    // after the delete. Every conversation's `state` is `working`, so recent
    // activity is the signal.
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    let now = chrono::Utc::now();

    let err = delete(h, &id, false, now, Confirm::Yes).unwrap_err();
    assert!(err.to_string().contains("--force"), "{err}");
    assert!(hist.exists() && index_has(h, &id) && head(h, &id).exists());
}

#[test]
fn force_overrides_the_recent_activity_guard() {
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    let now = chrono::Utc::now();

    delete(h, &id, true, now, Confirm::Yes).unwrap();
    assert!(!hist.exists() && !index_has(h, &id) && !head(h, &id).exists());
}
