//! `build_approval` walks the verdict table exactly as murmur's card did
//! (SSH setup spec, "Step 2: host", "Step 3: summary").

use mur_common::agent::SshKnownHost;

use super::super::tests::fp;
use super::choices::{Choices, NotReady, build_approval};
use super::probe::AgentKey;
use super::{ScannedKey, SetupPlan, SpawnChange, Verdict};

fn scanned(c: char) -> ScannedKey {
    ScannedKey {
        fingerprint: fp(c),
        entry: SshKnownHost {
            host: "gh.example".into(),
            key_type: "ssh-ed25519".into(),
            key_base64: format!("AAAA{c}"),
        },
    }
}

fn key(c: char) -> AgentKey {
    AgentKey {
        fingerprint: fp(c),
        comment: format!("key-{c}"),
        path: None,
    }
}

fn plan(verdict: Verdict, spawn: Option<SpawnChange>) -> SetupPlan {
    let matched = if verdict == Verdict::Trusted {
        vec![fp('H')]
    } else {
        vec![]
    };
    SetupPlan {
        agent: "qa".into(),
        host: mur_ssh_request::RequestHost::parse("gh.example", None).unwrap(),
        user: Some("git".into()),
        keys_loaded: vec![key('K')],
        keys_unloaded: vec![key('U')],
        scanned: vec![scanned('H'), scanned('J')],
        verdict,
        matched,
        spawn,
        exec_surface: None,
    }
}

fn with_key(c: char) -> Choices {
    Choices {
        key_fp: Some(fp(c)),
        ..Choices::default()
    }
}

#[test]
fn trusted_grants_only_the_matched_key() {
    let a = build_approval(&plan(Verdict::Trusted, None), &with_key('K')).unwrap();
    assert_eq!(a.key_fp, fp('K'));
    assert_eq!(a.key_label.as_deref(), Some("key-K"));
    let hosts: Vec<_> = a.hosts.iter().map(|h| h.host_key_fp.clone()).collect();
    assert_eq!(hosts, vec![fp('H')]);
    assert_eq!(a.hosts[0].label.as_deref(), Some("gh.example:22"));
    assert!(a.restart);
}

#[test]
fn key_problems_come_first() {
    let p = plan(Verdict::Trusted, None);
    assert_eq!(
        build_approval(&p, &Choices::default()).unwrap_err(),
        NotReady::NoKey
    );
    assert_eq!(
        build_approval(&p, &with_key('U')).unwrap_err(),
        NotReady::KeyNotLoaded
    );
    assert_eq!(
        build_approval(&p, &with_key('Z')).unwrap_err(),
        NotReady::NoKey
    );
}

#[test]
fn unknown_needs_a_tick_or_paste_and_grants_only_that() {
    let p = plan(Verdict::Unknown { downgraded: false }, None);
    assert!(matches!(
        build_approval(&p, &with_key('K')),
        Err(NotReady::Host(_))
    ));
    let ticked = Choices {
        ticked: vec![fp('J')],
        ..with_key('K')
    };
    let a = build_approval(&p, &ticked).unwrap();
    assert_eq!(a.hosts.len(), 1);
    assert_eq!(a.hosts[0].host_key_fp, fp('J'));
}

#[test]
fn differs_takes_only_a_paste_the_host_offered() {
    let p = plan(Verdict::Differs, None);
    let ticked = Choices {
        ticked: vec![fp('H')],
        ..with_key('K')
    };
    assert!(matches!(
        build_approval(&p, &ticked),
        Err(NotReady::Host(_))
    ));
    let wrong = Choices {
        pasted: Some(fp('Z')),
        ..with_key('K')
    };
    match build_approval(&p, &wrong) {
        Err(NotReady::Host(e)) => assert!(e.contains("did not offer"), "{e}"),
        other => panic!("{other:?}"),
    }
    let right = Choices {
        pasted: Some(fp('J')),
        ..with_key('K')
    };
    assert_eq!(
        build_approval(&p, &right).unwrap().hosts[0].host_key_fp,
        fp('J')
    );
}

#[test]
fn none_to_strict_needs_its_own_consent_and_add_ssh_does_not() {
    let p = plan(Verdict::Trusted, Some(SpawnChange::NoneToStrict));
    assert_eq!(
        build_approval(&p, &with_key('K')).unwrap_err(),
        NotReady::SpawnConsent
    );
    let ok = Choices {
        spawn_consented: true,
        ..with_key('K')
    };
    let a = build_approval(&p, &ok).unwrap();
    assert!(a.spawn_consented);
    assert_eq!(a.spawn, Some(SpawnChange::NoneToStrict));

    let p = plan(Verdict::Trusted, Some(SpawnChange::AddSsh));
    assert_eq!(
        build_approval(&p, &with_key('K')).unwrap().spawn,
        Some(SpawnChange::AddSsh)
    );
}

#[test]
fn a_bad_login_user_blocks_and_an_empty_one_does_not() {
    let p = plan(Verdict::Trusted, None);
    let bad = Choices {
        user: " x".into(),
        ..with_key('K')
    };
    assert!(matches!(build_approval(&p, &bad), Err(NotReady::User(_))));
    assert!(build_approval(&p, &with_key('K')).is_ok());
}

#[test]
fn not_ready_serializes_for_the_hub() {
    assert_eq!(
        serde_json::to_value(NotReady::NoKey).unwrap(),
        serde_json::json!({ "kind": "no_key" })
    );
    assert_eq!(
        serde_json::to_value(NotReady::Host("x".into())).unwrap(),
        serde_json::json!({ "kind": "host", "detail": "x" })
    );
}
