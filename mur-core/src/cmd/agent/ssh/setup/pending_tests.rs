//! Pending-request listing (moved from murmur's `ssh_card/tests.rs`, #2370
//! review) plus the two Hub entry points.

use chrono::Utc;
use mur_ssh_request::{RequestHost, SshRequest};

use super::super::status::{PairState, PairStatus, RequestStatus};
use super::super::tests::{fp, write_agent};
use super::front::overview;
use super::pending::{dismiss, open_everywhere, open_requests, still_open};

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

/// Grant `bot` a live allow for gh.example: the profile names the pair and
/// the agent's HITL log holds a signed allow, so the pair reads `Approved`.
fn grant_gh(home: &std::path::Path) {
    use mur_agent_runtime::hitl::{ssh_action_hash, store::ChannelDecisionStore};
    use mur_common::agent::{SshHostRule, SshKeyRule, SshKnownHost};
    use mur_common::identity::AgentIdentity;

    let mut p = write_agent(home, "bot");
    p.entitlements.ssh.keys = vec![SshKeyRule {
        fingerprint: fp('K'),
        label: None,
        allow_hosts: vec![SshHostRule {
            host_key: fp('H'),
            label: None,
            known_hosts: vec![SshKnownHost {
                host: "gh.example".into(),
                key_type: "ssh-ed25519".into(),
                key_base64: "AAAA".into(),
            }],
        }],
    }];
    let dir = home.join("agents").join("bot");
    std::fs::write(
        dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&p).unwrap(),
    )
    .unwrap();
    let id = std::sync::Arc::new(AgentIdentity::generate());
    id.save(&dir).unwrap();
    ChannelDecisionStore::new(home.to_path_buf(), "bot".into(), id, 0)
        .record_checked(&mur_common::hitl::HitlResponse {
            hitl_id: "h-1".into(),
            action_hash: ssh_action_hash("bot", &fp('K'), &fp('H')),
            allow: true,
            reason: String::new(),
            surface: "cli".into(),
            issued_at: Some(Utc::now()),
            label: None,
        })
        .unwrap();
}

/// Review of #2403: the `mur_home` argument is the only home these read.
/// `MUR_HOME` points elsewhere, so a profile read through the environment
/// would find no agent and drop the grant.
#[test]
fn the_mur_home_argument_wins_over_the_environment() {
    let mut env = mur_common::test_env::EnvGuard::hold();
    let elsewhere = tempfile::tempdir().unwrap();
    env.set_var("MUR_HOME", elsewhere.path());
    let tmp = tempfile::tempdir().unwrap();
    grant_gh(tmp.path());
    write_request(&tmp.path().join("agents").join("bot"), "deploy");

    let open = open_requests(tmp.path(), "bot");
    assert!(matches!(open.as_slice(), [o] if o.granted), "{open:?}");
    let ov = overview(&NoKeys, tmp.path(), "bot").unwrap();
    assert_eq!(ov.ssh.keys.len(), 1);
    assert!(matches!(ov.open.as_slice(), [o] if o.granted));
}

#[test]
fn open_everywhere_leaves_out_granted_requests() {
    let tmp = tempfile::tempdir().unwrap();
    grant_gh(tmp.path());
    write_request(&tmp.path().join("agents").join("bot"), "deploy");
    assert!(open_everywhere(tmp.path()).is_empty());
}

#[test]
fn overview_reads_grant_status_requests_and_loaded_keys() {
    let tmp = tempfile::tempdir().unwrap();
    write_agent(tmp.path(), "bot");
    write_request(&tmp.path().join("agents").join("bot"), "deploy");
    let ov = overview(&NoKeys, tmp.path(), "bot").unwrap();
    assert!(ov.ssh.keys.is_empty());
    assert!(ov.status.pairs.is_empty());
    assert!(matches!(ov.open.as_slice(), [o] if !o.granted && o.request.reason == "deploy"));
    assert_eq!(ov.loaded, vec![fp('L')]);
    assert!(overview(&NoKeys, tmp.path(), "ghost").is_err());
}

/// A pair index past the end (a status built for another profile) is
/// skipped, never a panic.
#[test]
fn still_open_ignores_an_out_of_range_pair() {
    let dir = tempfile::tempdir().unwrap();
    write_request(dir.path(), "deploy");
    let request = mur_ssh_request::read_pending(dir.path(), Utc::now()).remove(0);
    let open = still_open(
        dir.path(),
        &[],
        vec![RequestStatus {
            request,
            pairs: vec![7],
        }],
    );
    assert!(matches!(open.as_slice(), [o] if !o.granted));
}

struct NoKeys;

impl super::probe::Probe for NoKeys {
    fn agent_keys(&self) -> Vec<super::probe::AgentKey> {
        vec![super::probe::AgentKey {
            fingerprint: fp('L'),
            comment: String::new(),
            path: None,
        }]
    }
    fn unloaded_keys(&self) -> Vec<super::probe::AgentKey> {
        vec![]
    }
    fn scan(&self, _: &RequestHost) -> anyhow::Result<Vec<super::super::resolve::ScannedKey>> {
        Ok(vec![])
    }
    fn stored_host_keys(&self, _: &RequestHost) -> Vec<String> {
        vec![]
    }
    fn user_known_hosts(&self) -> Option<std::path::PathBuf> {
        None
    }
}
