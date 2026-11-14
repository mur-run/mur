//! Pending-request listing (moved from murmur's `ssh_card/tests.rs`, #2370
//! review) plus the two Hub entry points.

use chrono::Utc;
use mur_ssh_request::{RequestHost, SshRequest};

use super::super::status::{PairState, PairStatus, RequestStatus};
use super::super::tests::{fp, write_agent};
use super::pending::{dismiss, open_everywhere, still_open};

fn pair(state: PairState) -> PairStatus {
    PairStatus {
        key_fp: fp('K'),
        host_key_fp: fp('H'),
        host_names: vec!["gh.example".into()],
        action_hash: "h".into(),
        state,
        last_expired_mark: None,
    }
}

fn write_request(agent_home: &std::path::Path, reason: &str) {
    let t = RequestHost::parse("gh.example", None).unwrap();
    let req = SshRequest::new(&t, Some("git"), reason, Utc::now()).unwrap();
    mur_ssh_request::write(agent_home, &req).unwrap();
}

/// #2370 review: a signed request is deleted and never listed; an approved
/// one stays but is marked granted, so it neither reopens a card nor shows
/// on attach; a host with no grant stays open.
#[test]
fn cleared_requests_are_removed_and_granted_ones_do_not_reopen() {
    let dir = tempfile::tempdir().unwrap();
    write_request(dir.path(), "deploy");
    let r = || mur_ssh_request::read_pending(dir.path(), Utc::now()).remove(0);
    let reqs = |pairs: Vec<usize>| {
        vec![RequestStatus {
            request: r(),
            pairs,
        }]
    };

    let open = still_open(dir.path(), &[], reqs(vec![]));
    assert!(matches!(open.as_slice(), [o] if !o.granted));

    let approved = [pair(PairState::Approved)];
    let open = still_open(dir.path(), &approved, reqs(vec![0]));
    assert!(matches!(open.as_slice(), [o] if o.granted));

    let signed = [pair(PairState::Signed { at: Utc::now() })];
    assert!(still_open(dir.path(), &signed, reqs(vec![0])).is_empty());
    assert!(
        mur_ssh_request::read_pending(dir.path(), Utc::now()).is_empty(),
        "a signing mark removes the request file"
    );
}

#[test]
fn open_everywhere_lists_only_agents_with_open_requests() {
    let mut env = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    env.set_var("MUR_HOME", tmp.path());
    write_agent(tmp.path(), "bot");
    write_agent(tmp.path(), "quiet");
    write_request(&tmp.path().join("agents").join("bot"), "deploy");

    let all = open_everywhere(tmp.path());
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].0, "bot");
    assert_eq!(all[0].1.request.reason, "deploy");
    assert!(!all[0].1.granted);
}

#[test]
fn dismiss_removes_only_the_named_target() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let agent_home = home.join("agents").join("bot");
    std::fs::create_dir_all(&agent_home).unwrap();
    write_request(&agent_home, "deploy");

    let other = RequestHost::parse("other.example", None).unwrap();
    assert!(!dismiss(home, "bot", &other).unwrap());
    assert_eq!(
        mur_ssh_request::read_pending(&agent_home, Utc::now()).len(),
        1
    );

    let t = RequestHost::parse("gh.example", None).unwrap();
    assert!(dismiss(home, "bot", &t).unwrap());
    assert!(mur_ssh_request::read_pending(&agent_home, Utc::now()).is_empty());
}
