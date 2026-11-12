use std::sync::Arc;

use chrono::{Duration, Utc};
use mur_agent_runtime::hitl::store::ChannelDecisionStore;
use mur_channel::ChannelService;
use mur_common::agent::{SshHostRule, SshKeyRule, SshKnownHost};
use mur_common::hitl::HitlResponse;
use mur_ssh_request::{RequestHost, SshRequest};
use tempfile::TempDir;

use super::*;

const AGENT: &str = "qa";

fn fp(c: char) -> String {
    format!("SHA256:{}", c.to_string().repeat(43))
}

struct Fixture {
    _dir: TempDir,
    home: std::path::PathBuf,
    identity: Arc<AgentIdentity>,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let identity = Arc::new(AgentIdentity::generate());
        identity.save(&home.join("agents").join(AGENT)).unwrap();
        Self {
            _dir: dir,
            home,
            identity,
        }
    }

    fn agent_home(&self) -> std::path::PathBuf {
        self.home.join("agents").join(AGENT)
    }

    fn store(&self) -> ChannelDecisionStore {
        ChannelDecisionStore::new(self.home.clone(), AGENT.into(), self.identity.clone(), 0)
    }

    fn decide(&self, hash: &str, allow: bool, at: DateTime<Utc>) {
        self.store()
            .record_checked(&HitlResponse {
                hitl_id: format!("h-{}", at.timestamp_micros()),
                action_hash: hash.into(),
                allow,
                reason: String::new(),
                surface: "cli".into(),
                issued_at: Some(at),
                label: None,
            })
            .unwrap();
    }

    fn start_channel(&self, svc: &ChannelService) -> String {
        let marker = self.agent_home().join(START_CHANNEL_FILE);
        if let Ok(id) = std::fs::read_to_string(&marker) {
            return id;
        }
        let ch = svc.create_for_agent(AGENT).unwrap();
        std::fs::write(&marker, &ch.id).unwrap();
        ch.id
    }

    fn append(&self, payload: serde_json::Value, signed: bool) {
        let svc = ChannelService::open(&self.home).unwrap();
        let id = self.start_channel(&svc);
        let actor = ChannelActor::Agent { id: AGENT.into() };
        if signed {
            svc.append_signed(
                &id,
                &self.identity,
                0,
                actor,
                EventKind::Note,
                payload,
                None,
            )
            .unwrap();
        } else {
            svc.append(&id, actor, EventKind::Note, payload, None)
                .unwrap();
        }
    }

    fn mark(&self, hash: &str, issued_at: DateTime<Utc>) {
        let m = SigningMark::new(hash.into(), issued_at);
        self.append(serde_json::to_value(m).unwrap(), true);
    }

    fn request(&self, host: &str, port: u16) {
        let target = RequestHost::parse(host, Some(port)).unwrap();
        let req = SshRequest::new(&target, None, "deploy", Utc::now()).unwrap();
        mur_ssh_request::write(&self.agent_home(), &req).unwrap();
    }

    fn status(&self) -> SshStatus {
        status_at(&self.home, AGENT, &grant(), Utc::now())
    }
}

fn grant() -> SshEntitlement {
    SshEntitlement {
        keys: vec![SshKeyRule {
            fingerprint: fp('k'),
            label: None,
            allow_hosts: vec![SshHostRule {
                host_key: fp('h'),
                label: None,
                known_hosts: vec![SshKnownHost {
                    host: "[build.example]:2222".into(),
                    key_type: "ssh-ed25519".into(),
                    key_base64: "AAAA".into(),
                }],
            }],
        }],
    }
}

fn hash() -> String {
    ssh_action_hash(AGENT, &fp('k'), &fp('h'))
}

fn state(f: &Fixture) -> PairState {
    let s = f.status();
    assert_eq!(s.decisions_unverified, None);
    s.pairs[0].state.clone()
}

#[test]
fn nothing_written_reads_not_approved_and_creates_nothing() {
    let f = Fixture::new();
    assert_eq!(state(&f), PairState::NotApproved);
    assert!(!f.agent_home().join(HITL_CHANNEL_FILE).exists());
    assert!(!f.agent_home().join(START_CHANNEL_FILE).exists());
    // Opening the channel service would create (and migrate) the index.
    assert!(!f.home.join("index").exists());
}

#[test]
fn signed_status_reads_without_a_usable_index() {
    let f = Fixture::new();
    let at = Utc::now();
    f.decide(&hash(), true, at);
    f.mark(&hash(), at);
    // Make the index impossible to open: `index` is a file, so nothing can
    // create `index/channels/` under it. The channel files are untouched.
    let index = f.home.join("index");
    std::fs::remove_dir_all(&index).unwrap();
    std::fs::write(&index, b"not a directory").unwrap();
    assert!(ChannelService::open(&f.home).is_err());

    assert!(matches!(state(&f), PairState::Signed { .. }));
    assert!(index.is_file(), "the reader must not touch the index");
}

#[test]
fn allow_without_mark_is_approved() {
    let f = Fixture::new();
    f.decide(&hash(), true, Utc::now());
    assert_eq!(state(&f), PairState::Approved);
}

#[test]
fn signed_mark_under_live_allow_is_signed() {
    let f = Fixture::new();
    let at = Utc::now();
    f.decide(&hash(), true, at);
    f.mark(&hash(), at);
    assert!(matches!(state(&f), PairState::Signed { .. }));
}

#[test]
fn unsigned_mark_never_counts() {
    let f = Fixture::new();
    let at = Utc::now();
    f.decide(&hash(), true, at);
    let m = SigningMark::new(hash(), at);
    f.append(serde_json::to_value(m).unwrap(), false);
    assert_eq!(state(&f), PairState::Approved);
}

#[test]
fn wrong_notice_never_counts() {
    let f = Fixture::new();
    let at = Utc::now();
    f.decide(&hash(), true, at);
    let mut v = serde_json::to_value(SigningMark::new(hash(), at)).unwrap();
    v["notice"] = "ssh_proxy_refusal".into();
    f.append(v, true);
    assert_eq!(state(&f), PairState::Approved);
}

#[test]
fn deny_decision_is_not_approved_even_with_a_mark() {
    let f = Fixture::new();
    let at = Utc::now();
    f.decide(&hash(), false, at);
    f.mark(&hash(), at);
    assert_eq!(state(&f), PairState::NotApproved);
}

#[test]
fn mark_for_a_hash_with_no_decision_counts_for_nothing() {
    let f = Fixture::new();
    f.mark(&hash(), Utc::now());
    let s = f.status();
    assert_eq!(s.pairs[0].state, PairState::NotApproved);
    assert_eq!(s.pairs[0].last_expired_mark, None);
}

#[test]
fn reapproval_after_forget_does_not_inherit_the_old_mark() {
    let f = Fixture::new();
    let first = Utc::now() - Duration::seconds(60);
    f.decide(&hash(), true, first);
    f.mark(&hash(), first);
    f.store().forget(Some(&hash())).unwrap();
    f.decide(&hash(), true, Utc::now());
    let s = f.status();
    assert_eq!(s.pairs[0].state, PairState::Approved);
    // The old mark survives only as history.
    assert!(s.pairs[0].last_expired_mark.is_some());
}

#[test]
fn forgotten_approval_mark_is_history_not_state() {
    let f = Fixture::new();
    let at = Utc::now() - Duration::seconds(60);
    f.decide(&hash(), true, at);
    f.mark(&hash(), at);
    f.store().forget(Some(&hash())).unwrap();
    let s = f.status();
    assert_eq!(s.pairs[0].state, PairState::NotApproved);
    assert!(s.pairs[0].last_expired_mark.is_some());
}

#[test]
fn damaged_decision_log_trusts_no_decision() {
    let f = Fixture::new();
    let at = Utc::now();
    f.decide(&hash(), true, at);
    f.mark(&hash(), at);
    let id = std::fs::read_to_string(f.agent_home().join(HITL_CHANNEL_FILE)).unwrap();
    let log = mur_channel::ChannelStore::new(&f.home)
        .events_path(id.trim())
        .unwrap();
    std::fs::write(&log, "").unwrap();
    let s = f.status();
    assert!(s.decisions_unverified.is_some());
    assert_eq!(s.pairs[0].state, PairState::NotApproved);
}

#[test]
fn pending_request_maps_to_the_pair_naming_its_host_port() {
    let f = Fixture::new();
    f.request("build.example", 2222);
    f.request("other.example", 22);
    let s = f.status();
    let by_host = |h: &str| {
        s.requests
            .iter()
            .find(|r| r.request.target.host() == h)
            .unwrap()
    };
    assert_eq!(by_host("build.example").pairs, vec![0]);
    assert!(by_host("other.example").pairs.is_empty());
    assert!(!by_host("build.example").cleared(&s.pairs));
}

#[test]
fn a_mark_clears_its_request() {
    let f = Fixture::new();
    let at = Utc::now();
    f.decide(&hash(), true, at);
    f.mark(&hash(), at);
    f.request("build.example", 2222);
    let s = f.status();
    assert!(s.requests[0].cleared(&s.pairs));
}

#[test]
fn list_render_names_state_and_hides_cleared_requests() {
    let f = Fixture::new();
    let at = Utc::now();
    f.decide(&hash(), true, at);
    f.request("build.example", 2222);
    f.request("other.example", 22);
    let out = super::super::view::render_status(&f.status());
    assert!(out.contains("approved, no signing seen yet"), "{out}");
    assert!(out.contains("build.example"), "{out}");
    assert!(out.contains("[not granted]"), "{out}");
    assert!(!out.contains("connected"), "{out}");

    f.mark(&hash(), at);
    let out = super::super::view::render_status(&f.status());
    assert!(out.contains("signed under your approval"), "{out}");
    assert!(
        !out.contains("build.example"),
        "a cleared request stays listed: {out}"
    );
    assert!(out.contains("other.example"), "{out}");
}
