//! End-to-end over a temp `MUR_HOME`: the profile, pin and `known_hosts`
//! written together, and the `--yes` contract.

use std::fs;
use std::path::Path;

use mur_common::agent::{AgentProfile, SshKnownHost, known_hosts_path};
use mur_common::entitlements_pin::{self, PinCheck};

use super::edit::{Grant, HostGrant, RevokeScope};
use super::*;

pub(super) fn fp(c: char) -> String {
    format!("SHA256:{}", c.to_string().repeat(43))
}

pub(super) fn write_agent(home: &Path, name: &str) -> AgentProfile {
    let dir = home.join("agents").join(name);
    fs::create_dir_all(&dir).unwrap();
    let p = AgentProfile::default_for_tests();
    fs::write(
        dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&p).unwrap(),
    )
    .unwrap();
    entitlements_pin::write_pin(home, name, &p.entitlements).unwrap();
    p
}

pub(super) fn on_disk(home: &Path, name: &str) -> AgentProfile {
    let y = fs::read_to_string(home.join("agents").join(name).join("profile.yaml")).unwrap();
    serde_yaml_ng::from_str(&y).unwrap()
}

pub(super) fn named_grant(key: char, host: char, name: &str) -> Grant {
    Grant {
        key_fp: fp(key),
        key_label: None,
        host: Some(HostGrant {
            host_key_fp: fp(host),
            label: Some(name.into()),
            known_host: Some(SshKnownHost {
                host: name.into(),
                key_type: "ssh-ed25519".into(),
                key_base64: "AAAA".into(),
            }),
        }),
    }
}

#[test]
fn allow_writes_profile_pin_and_known_hosts_together() {
    let mut env = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    env.set_var("MUR_HOME", home);
    write_agent(home, "bot");

    commit::apply("bot", |e| {
        edit::allow(e, &named_grant('A', 'H', "gh.example"));
        Ok(())
    })
    .unwrap();

    let p = on_disk(home, "bot");
    assert_eq!(p.entitlements.ssh.keys[0].fingerprint, fp('A'));
    // What the supervisor's `pin::verify` checks before it starts the agent.
    assert_eq!(
        entitlements_pin::check(home, "bot", &p.entitlements).unwrap(),
        PinCheck::Match
    );
    let kh = fs::read_to_string(known_hosts_path(&home.join("agents/bot"))).unwrap();
    assert!(kh.contains("gh.example ssh-ed25519 AAAA"), "{kh}");

    commit::apply("bot", |e| {
        edit::revoke(e, &RevokeScope::new(Some(fp('A')), Some(fp('H'))).unwrap()).map(|_| ())
    })
    .unwrap();
    let p = on_disk(home, "bot");
    assert_eq!(
        entitlements_pin::check(home, "bot", &p.entitlements).unwrap(),
        PinCheck::Match
    );
    assert_eq!(
        fs::read_to_string(known_hosts_path(&home.join("agents/bot"))).unwrap(),
        ""
    );
}

#[test]
fn a_hand_edited_profile_is_refused_not_laundered_into_the_pin() {
    let mut env = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    env.set_var("MUR_HOME", home);
    let mut p = write_agent(home, "bot");
    // An ssh-only edit made outside MUR.
    p.entitlements.ssh.keys.push(mur_common::agent::SshKeyRule {
        fingerprint: fp('X'),
        label: None,
        allow_hosts: Vec::new(),
    });
    let path = home.join("agents/bot/profile.yaml");
    fs::write(&path, serde_yaml_ng::to_string(&p).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();

    let err = commit::apply("bot", |e| {
        edit::allow(e, &named_grant('A', 'H', "gh.example"));
        Ok(())
    })
    .unwrap_err();
    assert!(format!("{err:#}").contains("reseal"), "{err:#}");
    assert_eq!(fs::read(&path).unwrap(), before, "profile untouched");
    assert!(!known_hosts_path(&home.join("agents/bot")).exists());
}

#[test]
fn a_failing_edit_writes_nothing() {
    let mut env = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    env.set_var("MUR_HOME", home);
    write_agent(home, "bot");
    let path = home.join("agents/bot/profile.yaml");
    let before = fs::read(&path).unwrap();
    let r = commit::apply("bot", |e| {
        edit::revoke(e, &RevokeScope::new(Some(fp('Z')), None).unwrap()).map(|_| ())
    });
    assert!(r.is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn a_known_hosts_install_failure_rolls_back_profile_and_pin() {
    let mut env = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    env.set_var("MUR_HOME", home);
    write_agent(home, "bot");
    let path = home.join("agents/bot/profile.yaml");
    let pin = entitlements_pin::pin_path(home, "bot");
    let (before, pin_before) = (fs::read(&path).unwrap(), fs::read(&pin).unwrap());
    // A directory where the file must go: the final rename fails.
    let kh = known_hosts_path(&home.join("agents/bot"));
    fs::create_dir_all(kh.join("blocker")).unwrap();

    let r = commit::apply("bot", |e| {
        edit::allow(e, &named_grant('A', 'H', "gh.example"));
        Ok(())
    });
    assert!(r.is_err());
    assert_eq!(fs::read(&path).unwrap(), before, "profile restored");
    assert_eq!(fs::read(&pin).unwrap(), pin_before, "pin restored");
}

fn args<'a>(key: &'a str, host: Option<&'a str>, host_key: Option<&'a str>) -> AllowArgs<'a> {
    AllowArgs {
        agent: "bot",
        key,
        label: None,
        host,
        port: None,
        host_key,
        host_label: None,
        yes: true,
        no_restart: false,
    }
}

#[test]
fn yes_means_known_fingerprints_never_a_lookup() {
    let (k, h) = (fp('A'), fp('H'));
    assert!(check_yes_contract(&args(&k, None, Some(&h))).is_ok());
    assert!(check_yes_contract(&args(&k, Some("github.com"), Some(&h))).is_err());
    assert!(check_yes_contract(&args(&k, None, None)).is_err());
    assert!(check_yes_contract(&args("deploy@ci", None, Some(&h))).is_err());
    let mut attended = args("deploy@ci", Some("github.com"), None);
    attended.yes = false;
    assert!(check_yes_contract(&attended).is_ok());
}
