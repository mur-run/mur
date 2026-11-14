use mur_common::agent::SshKnownHost;
use mur_core::a2a_dial::StepEvent;
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
    assert_eq!(d.differs_cmd.as_deref(), Some("ssh-keygen -R gh.example"));
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
