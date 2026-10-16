//! Fixtures are built the way production builds them: the router identity is
//! planted under `<home>/agents/mur/` and events go through `append_as_writer`,
//! which anchors the head for a disk-loaded key. Damage is then done to the
//! files on disk, the way an attacker or a bad disk would.

use super::*;
use crate::channel_writer::append_as_writer;
use mur_channel::ChannelService;
use mur_common::channel::EventKind;
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
    svc: ChannelService,
    id: String,
}

impl Fixture {
    /// A channel with `n` anchored events from a router that holds a key.
    fn anchored(n: usize) -> Self {
        let home = TempDir::new().unwrap();
        let agent_home = home.path().join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        AgentIdentity::generate().save(&agent_home).unwrap();
        let svc = ChannelService::open(home.path()).unwrap();
        let id = svc.create_for_workflow("verify").unwrap().id;
        let f = Self { home, svc, id };
        for i in 0..n {
            f.append(i);
        }
        f
    }

    /// A channel whose router has no key: events are unsigned, no head.
    fn unanchored(n: usize) -> Self {
        let home = TempDir::new().unwrap();
        let svc = ChannelService::open(home.path()).unwrap();
        let id = svc.create_for_workflow("verify").unwrap().id;
        let f = Self { home, svc, id };
        for i in 0..n {
            f.append(i);
        }
        f
    }

    fn append(&self, i: usize) {
        append_as_writer(
            &self.svc,
            self.home.path(),
            &self.id,
            "mur",
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": format!("event {i}") }),
            None,
        )
        .unwrap();
    }

    fn check(&self) -> IntegrityReport {
        check(self.home.path(), &self.id).unwrap()
    }

    fn log_path(&self) -> std::path::PathBuf {
        self.svc.store().events_path(&self.id)
    }

    fn head_path(&self) -> std::path::PathBuf {
        mur_channel::head_io::head_path(&mur_channel::anchor::heads_dir(self.home.path()), &self.id)
            .unwrap()
    }

    fn log_lines(&self) -> Vec<String> {
        std::fs::read_to_string(self.log_path())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn write_log_lines(&self, lines: &[String]) {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(self.log_path(), text).unwrap();
    }
}

#[test]
fn an_anchored_untouched_channel_is_intact() {
    let f = Fixture::anchored(3);
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(r.exit_code(), EXIT_INTACT);
    assert_eq!(r.events, 3);
    assert_eq!(r.anchored_lines, Some(3));
}

#[test]
fn a_channel_with_no_head_is_intact_but_says_it_is_unanchored() {
    let f = Fixture::unanchored(2);
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(r.anchored_lines, None);
    assert!(r.render().contains("not anchored"), "{}", r.render());
}

#[test]
fn a_channel_that_does_not_exist_is_an_error_not_a_clean_bill() {
    let home = TempDir::new().unwrap();
    let err = check(home.path(), "no-such-channel").unwrap_err();
    assert!(err.to_string().contains("no such channel"), "{err:#}");
}

#[test]
fn an_unsafe_channel_id_is_refused() {
    let home = TempDir::new().unwrap();
    for id in ["", ".", "..", "../x", "a/b", "a\\b"] {
        assert!(check(home.path(), id).is_err(), "accepted {id:?}");
    }
}

#[test]
fn deleting_the_last_line_is_truncation() {
    let f = Fixture::anchored(3);
    let mut lines = f.log_lines();
    lines.pop();
    f.write_log_lines(&lines);
    let r = f.check();
    assert_eq!(r.anchor_mismatch, Some(AnchorMismatchKind::Truncated));
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
    assert!(r.render().contains("fewer lines"), "{}", r.render());
}

#[test]
fn removing_the_whole_log_is_truncation() {
    let f = Fixture::anchored(2);
    std::fs::remove_file(f.log_path()).unwrap();
    let r = f.check();
    assert_eq!(r.anchor_mismatch, Some(AnchorMismatchKind::Truncated));
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
}

#[test]
fn editing_a_covered_line_is_a_rewrite() {
    let f = Fixture::anchored(3);
    let mut lines = f.log_lines();
    lines[0] = lines[0].replace("event 0", "event X");
    f.write_log_lines(&lines);
    let r = f.check();
    assert_eq!(r.anchor_mismatch, Some(AnchorMismatchKind::Rewritten));
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
}

#[test]
fn an_edited_head_fails_its_signature() {
    let f = Fixture::anchored(2);
    let mut head: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.head_path()).unwrap()).unwrap();
    head["seq"] = serde_json::json!(head["seq"].as_u64().unwrap() + 1);
    std::fs::write(f.head_path(), serde_json::to_vec(&head).unwrap()).unwrap();
    let r = f.check();
    assert_eq!(r.anchor_mismatch, Some(AnchorMismatchKind::BadSignature));
}

#[test]
fn a_head_copied_from_another_channel_is_the_wrong_channel() {
    let a = Fixture::anchored(2);
    let b_id = a.svc.create_for_workflow("other").unwrap().id;
    for i in 0..2 {
        append_as_writer(
            &a.svc,
            a.home.path(),
            &b_id,
            "mur",
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": format!("b {i}") }),
            None,
        )
        .unwrap();
    }
    let b_head =
        mur_channel::head_io::head_path(&mur_channel::anchor::heads_dir(a.home.path()), &b_id)
            .unwrap();
    std::fs::copy(a.head_path(), &b_head).unwrap();
    let r = check(a.home.path(), &b_id).unwrap();
    assert_eq!(r.anchor_mismatch, Some(AnchorMismatchKind::WrongChannel));
}

#[test]
fn lines_written_past_the_head_are_not_damage() {
    let f = Fixture::anchored(2);
    // A sandboxed agent appends signed but cannot anchor; `append_signed` is
    // that path (no head write), so the log grows past the head.
    let agent_home = f.home.path().join("agents").join("mur");
    let id = AgentIdentity::load(&agent_home).unwrap();
    f.svc
        .append_signed(
            &f.id,
            &id,
            1,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": "past the head" }),
            None,
        )
        .unwrap();
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(r.events, 3);
    assert_eq!(r.anchored_lines, Some(2));
    assert!(r.render().contains("1 written past it"), "{}", r.render());
}

#[test]
fn a_dirty_marker_is_reported() {
    let f = Fixture::anchored(2);
    mur_channel::anchor::mark_dirty(f.home.path(), &f.id).unwrap();
    let r = f.check();
    assert!(r.anchor_dirty);
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
    assert!(r.render().contains("stale"), "{}", r.render());
}

#[test]
fn a_garbage_line_is_reported_with_its_number() {
    let f = Fixture::anchored(2);
    let mut text = std::fs::read_to_string(f.log_path()).unwrap();
    text.push_str("this is not json\n");
    std::fs::write(f.log_path(), text).unwrap();
    let r = f.check();
    assert_eq!(r.unparseable_lines, vec![3]);
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
}

#[test]
fn an_unreadable_head_is_reported_not_treated_as_missing() {
    let f = Fixture::anchored(1);
    std::fs::write(f.head_path(), b"{ not a head").unwrap();
    let r = f.check();
    assert!(r.head_error.is_some());
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
    assert!(r.render().contains("unreadable"), "{}", r.render());
}

#[test]
fn an_event_signed_by_the_wrong_actors_key_is_flagged() {
    let f = Fixture::anchored(1);
    // Signed by the router, but claiming to be `worker`, whose key does not
    // exist: the per-actor check cannot vouch for it and fails closed.
    append_as_writer(
        &f.svc,
        f.home.path(),
        &f.id,
        "mur",
        ChannelActor::Agent {
            id: "worker".into(),
        },
        EventKind::Note,
        serde_json::json!({ "text": "forged" }),
        None,
    )
    .unwrap();
    let r = f.check();
    assert_eq!(r.unverified_seqs.len(), 1, "{}", r.render());
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
}

#[test]
fn a_removed_middle_line_breaks_the_chain_even_with_the_head_deleted_too() {
    // Signed (v2) events carry `prev_hash`. With the head gone, the chain is
    // the only thing left that can notice a line removed from the middle.
    let f = Fixture::anchored(4);
    let mut lines = f.log_lines();
    lines.remove(1);
    f.write_log_lines(&lines);
    std::fs::remove_file(f.head_path()).unwrap();
    let r = f.check();
    assert!(!r.chain_breaks.is_empty(), "{}", r.render());
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
    assert!(r.render().contains("broken hash chain"), "{}", r.render());
}

#[test]
fn a_chain_break_is_reported_even_when_a_head_exists() {
    let f = Fixture::anchored(4);
    let mut lines = f.log_lines();
    lines.remove(1);
    f.write_log_lines(&lines);
    let r = f.check();
    assert!(!r.chain_breaks.is_empty(), "{}", r.render());
    assert_eq!(r.exit_code(), EXIT_DAMAGED);
}

#[test]
fn an_unreadable_head_does_not_claim_the_channel_has_zero_events() {
    let f = Fixture::anchored(3);
    std::fs::write(f.head_path(), b"{ not a head").unwrap();
    let out = f.check().render();
    assert!(!out.contains("0 events"), "{out}");
    assert!(out.contains("3 lines"), "{out}");
}

#[test]
fn signed_events_with_no_head_stay_intact_under_the_inert_key() {
    // Locks the premise behind `NO_HEAD_INERT_KEY`: with no head, the loader
    // must not judge signed events against that all-zero key. If it ever
    // does, every signed event here turns up as damage.
    let f = Fixture::anchored(3);
    std::fs::remove_file(f.head_path()).unwrap();
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(r.events, 3);
}

#[test]
fn a_created_channel_with_no_events_is_intact_and_unanchored() {
    // `ChannelStore::create` writes only the directory and manifest; there is
    // no log yet. That is an existing, empty channel, not a missing one.
    let f = Fixture::unanchored(0);
    assert!(!f.log_path().exists(), "premise: no events file yet");
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(r.events, 0);
    assert_eq!(r.anchored_lines, None);
    assert_eq!(r.exit_code(), EXIT_INTACT);
}

#[test]
fn verify_does_not_create_the_channel_index() {
    let f = Fixture::anchored(2);
    let index = f.home.path().join("index");
    std::fs::remove_dir_all(&index).unwrap();
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert!(!index.exists(), "verify created {}", index.display());
}

#[test]
fn verify_works_without_a_usable_index() {
    // An index path that cannot be opened (a plain file where the directory
    // belongs) must not stop a check that only needs the log and the head.
    let f = Fixture::anchored(2);
    let index = f.home.path().join("index");
    std::fs::remove_dir_all(&index).unwrap();
    std::fs::write(&index, b"not a directory").unwrap();
    let r = f.check();
    assert!(r.is_intact(), "{}", r.render());
    assert_eq!(std::fs::read(&index).unwrap(), b"not a directory");
}
