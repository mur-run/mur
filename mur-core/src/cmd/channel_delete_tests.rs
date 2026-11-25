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

/// `agent`'s channel history dir, where the runtime keeps the history and
/// takes each turn's lease.
fn channel_hist_dir(home: &Path, name: &str) -> PathBuf {
    agent(home, name);
    let d = mur_channel::turn_lease::agent_history_dir(&home.join("agents").join(name));
    std::fs::create_dir_all(&d).unwrap();
    d
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

    let out = delete(h, &id, false, Confirm::Yes).unwrap();

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
    delete(h, &handle, false, Confirm::Yes).unwrap();
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

    let err = delete(h, &shared, false, Confirm::Yes).unwrap_err();

    assert!(err.to_string().contains("matches 2 channels"), "{err}");
    assert!(index_has(h, &a) && index_has(h, &b));
    assert!(ha.exists() && hb.exists());
}

#[test]
fn a_prefix_below_the_floor_is_refused() {
    let tmp = home();
    let err = delete(tmp.path(), "01a", false, Confirm::Yes).unwrap_err();
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

    let err = delete(h, &ch.id, true, Confirm::Yes).unwrap_err();

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

    let err = delete(h, &id, false, Confirm::Yes).unwrap_err();
    assert!(err.to_string().contains("pending approval"), "{err}");
    assert!(hist.exists() && index_has(h, &id));

    delete(h, &id, true, Confirm::Yes).unwrap();
    assert!(!hist.exists() && !index_has(h, &id));
}

#[test]
fn answering_no_keeps_everything() {
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    let mut input = answer("n\n");
    let out = delete(
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
    let err = delete(
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

    let Outcome::Deleted { histories, .. } = delete(h, &id, false, Confirm::Yes).unwrap() else {
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
fn a_running_turn_refuses_however_old_its_last_update() {
    // The reviewer's case: the turn's last channel update is long past, but
    // it is still in a model call. Its lease, not the clock, decides.
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    let dir = channel_hist_dir(h, "aura");
    let chist = dir.join(format!("{id}.json"));
    std::fs::write(&chist, b"{}").unwrap();
    let turn = mur_channel::turn_lease::begin_turn(&dir, &id).unwrap();

    let err = delete(h, &id, false, Confirm::Yes).unwrap_err();

    assert!(err.to_string().contains("turn is running"), "{err}");
    assert!(hist.exists() && chist.exists());
    assert!(index_has(h, &id) && head(h, &id).exists());
    drop(turn);
}

#[test]
fn force_does_not_override_a_running_turn() {
    // --force skips the approval check; it cannot stop the running turn
    // from writing its history back, so it must not pretend to.
    let tmp = home();
    let h = tmp.path();
    let (id, _) = conversation(h);
    let dir = channel_hist_dir(h, "aura");
    let _turn = mur_channel::turn_lease::begin_turn(&dir, &id).unwrap();

    let err = delete(h, &id, true, Confirm::Yes).unwrap_err();
    assert!(err.to_string().contains("turn is running"), "{err}");
    assert!(index_has(h, &id));
}

#[test]
fn an_ended_turn_does_not_block_and_its_files_go() {
    let tmp = home();
    let h = tmp.path();
    let (id, _) = conversation(h);
    let dir = channel_hist_dir(h, "aura");
    let chist = dir.join(format!("{id}.json"));
    std::fs::write(&chist, b"{}").unwrap();
    drop(mur_channel::turn_lease::begin_turn(&dir, &id).unwrap());
    let lease = mur_channel::turn_lease::lease_path(&dir, &id).unwrap();
    assert!(lease.exists());

    let Outcome::Deleted { histories, .. } = delete(h, &id, false, Confirm::Yes).unwrap() else {
        panic!("not deleted");
    };

    assert!(histories.contains(&chist), "{histories:?}");
    assert!(!chist.exists() && !lease.exists() && !index_has(h, &id));
}

#[test]
fn a_turn_admitted_during_the_delete_finds_the_channel_gone() {
    // The runtime takes the lease before its locked read; a delete holding
    // it makes that turn wait, and the read after it sees no channel.
    let tmp = home();
    let h = tmp.path().to_path_buf();
    let (id, _) = conversation(&h);
    let dir = channel_hist_dir(&h, "aura");
    let (tx, rx) = std::sync::mpsc::channel();
    let mut input = answer("y\n");
    // Confirmation runs with the leases held: start a turn there.
    let out = {
        let (d2, i2, h2) = (dir.clone(), id.clone(), h.clone());
        let mut reader = HookedReader {
            inner: &mut input,
            hook: Some(Box::new(move || {
                std::thread::spawn(move || {
                    let _l = mur_channel::turn_lease::begin_turn(&d2, &i2).unwrap();
                    tx.send(svc(&h2).store().has_manifest(&i2)).unwrap();
                });
            })),
        };
        delete(
            &h,
            &id,
            false,
            Confirm::Ask {
                input: &mut reader,
                interactive: true,
            },
        )
        .unwrap()
    };
    assert!(matches!(out, Outcome::Deleted { .. }));
    let saw_channel = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(!saw_channel, "turn was admitted before the delete finished");
}

/// Runs `hook` once, on the first read (the confirmation prompt).
struct HookedReader<'a> {
    inner: &'a mut dyn std::io::BufRead,
    hook: Option<Box<dyn FnOnce()>>,
}

impl std::io::Read for HookedReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl std::io::BufRead for HookedReader<'_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if let Some(h) = self.hook.take() {
            h();
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        self.inner.fill_buf()
    }
    fn consume(&mut self, n: usize) {
        self.inner.consume(n)
    }
}

#[test]
fn a_conversation_just_written_is_deletable_when_no_turn_runs() {
    // No time window: recency alone refuses nothing.
    let tmp = home();
    let h = tmp.path();
    let (id, hist) = conversation(h);
    delete(h, &id, false, Confirm::Yes).unwrap();
    assert!(!hist.exists() && !index_has(h, &id) && !head(h, &id).exists());
}
