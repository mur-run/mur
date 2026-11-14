use mur_common::agent::SshKnownHost;
use mur_core::a2a_dial::StepEvent;
use mur_core::cmd::agent::ssh_setup::plan::Dedicated;
use mur_core::cmd::agent::ssh_setup::probe::AgentKey;
use mur_core::cmd::agent::ssh_setup::{ScannedKey, SetupPlan, SpawnChange, Verdict, front};
use std::collections::HashSet;

use super::view::{plan_view, sh_quote};
use super::{MAX_PLANS, SshPlans, is_ssh_request_done};

fn fp(c: char) -> String {
    format!("SHA256:{}", c.to_string().repeat(43))
}

fn plan(verdict: Verdict, spawn: Option<SpawnChange>) -> SetupPlan {
    SetupPlan {
        agent: "qa".into(),
        host: front::parse_target("gh.example", Some(2222)).unwrap(),
        user: None,
        keys_loaded: vec![AgentKey {
            fingerprint: fp('K'),
            comment: "laptop".into(),
            path: None,
        }],
        keys_unloaded: vec![AgentKey {
            fingerprint: fp('U'),
            comment: String::new(),
            path: Some("/home/u/.ssh/id work.pub".into()),
        }],
        scanned: vec![ScannedKey {
            fingerprint: fp('H'),
            entry: SshKnownHost {
                host: "[gh.example]:2222".into(),
                key_type: "ssh-ed25519".into(),
                key_base64: "AAAAH".into(),
            },
        }],
        verdict,
        matched: vec![],
        spawn,
        exec_surface: None,
        dedicated: Dedicated::Offer,
    }
}

#[test]
fn plan_view_carries_names_not_rules() {
    let v = plan_view(
        "id-1",
        &plan(
            Verdict::Unknown { downgraded: true },
            Some(SpawnChange::NoneToStrict),
        ),
        Some("deploy"),
    );
    assert_eq!(v.plan_id, "id-1");
    assert_eq!(v.host, "gh.example:2222");
    assert_eq!(v.verdict, "unknown_downgraded");
    assert!(v.ticks_offered);
    assert_eq!(v.spawn, Some("none_to_strict"));
    assert_eq!(v.reason.as_deref(), Some("deploy"));
    assert_eq!(v.keys.len(), 2);
    assert!(v.keys[0].loaded && v.keys[0].add_cmd.is_none());
    assert_eq!(
        v.keys[1].add_cmd.as_deref(),
        Some("ssh-add '/home/u/.ssh/id work'"),
        "the private key, quoted, never the .pub"
    );
    assert_eq!(v.differs_cmd, None);

    let d = plan_view("id-2", &plan(Verdict::Differs, None), None);
    assert!(!d.ticks_offered);
    assert_eq!(
        d.differs_cmd.as_deref(),
        Some("ssh-keygen -R '[gh.example]:2222'"),
        "a non-default port is stored as [host]:port"
    );
}

#[test]
fn quoting_leaves_plain_paths_alone() {
    assert_eq!(
        sh_quote("/home/u/.ssh/id_ed25519"),
        "/home/u/.ssh/id_ed25519"
    );
    assert_eq!(sh_quote("/a b/it's"), "'/a b/it'\\''s'");
}

#[test]
fn plans_are_capped_and_a_missing_one_says_how_to_recover() {
    let plans = SshPlans::default();
    for _ in 0..MAX_PLANS + 1 {
        plans.put(plan(Verdict::Trusted, None), None, "x".into());
    }
    assert_eq!(plans.len(), MAX_PLANS);
    let e = plans.with("nope", |_| ()).unwrap_err();
    assert!(e.contains("open it again"), "{e}");
}

#[test]
fn only_a_finished_ssh_request_step_counts() {
    let started = |id: &str, name: &str| StepEvent::Started {
        step_id: id.into(),
        task_id: "t".into(),
        name: name.into(),
        args: serde_json::Value::Null,
    };
    let completed =
        |id: &str| mur_core::a2a_dial::parse_step(&serde_json::json!({ "step_id": id }), true);
    let mut seen = HashSet::new();
    assert!(!is_ssh_request_done(&mut seen, &started("1", "bash")));
    assert!(!is_ssh_request_done(&mut seen, &completed("1")));
    assert!(!is_ssh_request_done(
        &mut seen,
        &started("2", "ssh_request")
    ));
    assert!(is_ssh_request_done(&mut seen, &completed("2")));
    assert!(!is_ssh_request_done(&mut seen, &completed("2")), "once");
}

#[test]
fn a_plan_is_taken_once_and_a_put_back_can_be_taken_again() {
    let plans = SshPlans::default();
    let id = plans.put(plan(Verdict::Trusted, None), None, "x".into());
    let held = plans.take(&id).unwrap();
    let e = plans
        .take(&id)
        .err()
        .expect("a second Approve finds nothing");
    assert!(e.contains("open it again"), "{e}");
    plans.put_back(id.clone(), held);
    assert!(
        plans.take(&id).is_ok(),
        "nothing was written, so it can retry"
    );
}

#[test]
fn one_apply_per_agent_at_a_time() {
    let plans = SshPlans::default();
    let slot = plans.claim("qa").unwrap();
    let e = plans.claim("qa").err().expect("second apply refused");
    assert!(e.contains("already being applied"), "{e}");
    assert!(plans.claim("other").is_ok(), "a different agent is free");
    drop(slot);
    assert!(plans.claim("qa").is_ok(), "the slot frees on drop");
}

#[test]
fn not_ready_reads_as_words_not_debug() {
    use super::view::not_ready_text;
    use mur_core::cmd::agent::ssh_setup::choices::NotReady;
    assert!(!not_ready_text(&NotReady::KeyNotLoaded).contains("KeyNotLoaded"));
    assert_eq!(not_ready_text(&NotReady::Host("why".into())), "why");
}

/// A request as `read_pending` builds it (the type is non-exhaustive).
fn request(host: &str, port: u16, at: i64) -> mur_ssh_request::PendingRequest {
    let dir = tempfile::tempdir().unwrap();
    let target = front::parse_target(host, Some(port)).unwrap();
    let at = chrono::DateTime::from_timestamp(at, 0).unwrap();
    let req = mur_ssh_request::SshRequest::new(&target, Some("git"), "deploy", at).unwrap();
    mur_ssh_request::write(dir.path(), &req).unwrap();
    mur_ssh_request::read_pending(dir.path(), at)
        .pop()
        .expect("just written")
}

#[test]
fn overview_names_each_pair_state_and_the_loaded_key() {
    use mur_common::agent::{SshEntitlement, SshHostRule, SshKeyRule};
    use mur_core::cmd::agent::ssh_setup::pending::OpenRequest;
    use mur_core::cmd::agent::ssh_status::{PairState, PairStatus, SshStatus};

    let host = |c: char| SshHostRule {
        host_key: fp(c),
        label: None,
        known_hosts: vec![SshKnownHost {
            host: format!("{c}.example"),
            key_type: "ssh-ed25519".into(),
            key_base64: "AAAA".into(),
        }],
    };
    let pair = |c: char, state: PairState| PairStatus {
        key_fp: fp('K'),
        host_key_fp: fp(c),
        host_names: vec![],
        action_hash: String::new(),
        state,
        last_expired_mark: None,
    };
    let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let ov = front::Overview {
        ssh: SshEntitlement {
            keys: vec![SshKeyRule {
                fingerprint: fp('K'),
                label: Some("laptop".into()),
                allow_hosts: vec![host('A'), host('B'), host('C'), host('D')],
            }],
        },
        status: SshStatus {
            pairs: vec![
                pair('A', PairState::Signed { at }),
                pair('B', PairState::Approved),
                pair('C', PairState::NotApproved),
            ],
            requests: vec![],
            decisions_unverified: None,
        },
        open: vec![OpenRequest {
            request: request("gh.example", 2222, 1_700_000_000),
            granted: true,
        }],
        loaded: vec![fp('K')],
    };
    let v = super::view::overview_view("qa", &ov);
    let k = &v.keys[0];
    assert!(k.loaded);
    let states: Vec<_> = k.hosts.iter().map(|h| h.state).collect();
    assert_eq!(
        states,
        ["signed", "approved", "not_approved", "not_approved"],
        "a host with no status pair reads not_approved, never failed"
    );
    assert!(k.hosts[0].signed_at.is_some() && k.hosts[1].signed_at.is_none());
    assert_eq!(
        k.hosts[3].names,
        ["D.example"],
        "names fall back to the confirmed known_hosts entries"
    );
    let p = &v.pending[0];
    assert_eq!((p.agent.as_str(), p.port), ("qa", 2222));
    assert_eq!(p.canonical, "gh.example:2222");
    assert_eq!(p.requested_at, 1_700_000_000);
    assert!(p.granted);
}

#[test]
fn check_view_is_ready_or_says_what_is_missing() {
    use mur_core::cmd::agent::ssh_setup::choices::{Choices, NotReady, build_approval};
    let p = plan(Verdict::Unknown { downgraded: false }, None);
    let ok = super::view::check_view(build_approval(
        &p,
        &Choices {
            key_fp: Some(fp('K')),
            ticked: vec![fp('H')],
            ..Default::default()
        },
    ));
    let r = ok.ready.expect("a loaded key and a ticked host key");
    assert_eq!(r.key_fp, fp('K'));
    assert_eq!(r.host_keys, [fp('H')]);
    assert!(ok.not_ready.is_none());

    let unloaded = super::view::check_view(build_approval(
        &p,
        &Choices {
            key_fp: Some(fp('U')),
            ..Default::default()
        },
    ));
    assert!(unloaded.ready.is_none());
    assert_eq!(unloaded.not_ready, Some(NotReady::KeyNotLoaded));
    assert_eq!(
        serde_json::to_value(&unloaded).unwrap()["not_ready"]["kind"],
        "key_not_loaded"
    );
}

#[test]
fn pending_stamp_moves_when_a_request_is_written() {
    let home = tempfile::tempdir().unwrap();
    let agent_home = home.path().join("agents").join("qa");
    assert!(super::pending_stamp(home.path(), "qa").is_empty());
    // Recent times: an old request has expired and reads as gone.
    let now = chrono::Utc::now().timestamp();
    let write = |host: &str, at: i64| {
        let target = front::parse_target(host, Some(22)).unwrap();
        let at = chrono::DateTime::from_timestamp(at, 0).unwrap();
        let req = mur_ssh_request::SshRequest::new(&target, None, "deploy", at).unwrap();
        mur_ssh_request::write(&agent_home, &req).unwrap();
    };
    write("a.example", now - 100);
    let one = super::pending_stamp(home.path(), "qa");
    assert_eq!(one.len(), 1);
    assert_eq!(super::pending_stamp(home.path(), "qa"), one, "stable");
    write("a.example", now);
    assert_ne!(
        super::pending_stamp(home.path(), "qa"),
        one,
        "asking again for the same host still counts"
    );
}

#[test]
fn revoke_view_carries_restart_needed_beside_the_overview() {
    let v = super::view::RevokeView {
        overview: super::view::OverviewView {
            keys: vec![],
            pending: vec![],
            decisions_unverified: None,
        },
        restart_needed: true,
    };
    let j = serde_json::to_value(&v).unwrap();
    assert_eq!(j["restart_needed"], true);
    assert!(j["overview"]["keys"].is_array());
}
