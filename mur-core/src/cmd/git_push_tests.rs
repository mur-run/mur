use super::*;
use mur_common::agent::AgentProfile;

struct Fx {
    _t: tempfile::TempDir,
    home: PathBuf,
    remote: PathBuf,
    repo: PathBuf,
}

fn write_profile(home: &Path, name: &str, fail_closed: bool) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut p = AgentProfile {
        name: name.to_string(),
        ..AgentProfile::default_for_tests()
    };
    p.entitlements.fail_closed_on_sandbox_error = fail_closed;
    p.entitlements.filesystem.write.clear();
    std::fs::write(
        dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&p).unwrap(),
    )
    .unwrap();
}

fn fx() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(t.path()).unwrap();
    let home = root.join("mur");
    let remote = root.join("remote.git");
    let repo = root.join("work/mur");
    for d in [&home, &remote, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    write_profile(&home, "alice", true);
    write_profile(&home, "bob", true);
    plant_router(&home);
    Fx {
        _t: t,
        home,
        remote,
        repo,
    }
}

fn enroll_args(f: &Fx, agent: &str) -> GitPushAction {
    GitPushAction::Enroll {
        agent: agent.into(),
        repo: "mur".into(),
        remote: "origin".into(),
        endpoint: format!("file://{}", f.remote.display()),
        base_refs: vec![],
        ref_prefix: None,
        path: Some(f.repo.clone()),
        object_format: None,
        auth: EnrollAuth::default(),
    }
}

fn go(f: &Fx, a: GitPushAction) -> (i32, String) {
    let mut out = Vec::new();
    let code = run_to(&f.home, a, &mut out).unwrap();
    (code, String::from_utf8(out).unwrap())
}

fn files(f: &Fx) -> (Option<String>, Option<String>) {
    (
        std::fs::read_to_string(enrollments_path(&f.home)).ok(),
        std::fs::read_to_string(registry_path(&f.home)).ok(),
    )
}

#[test]
fn enroll_writes_both_files_with_a_per_agent_prefix_and_no_base_refs() {
    let f = fx();
    let (code, out) = go(&f, enroll_args(&f, "alice"));
    assert_eq!(code, 0, "{out}");
    let book = load(&f.home).unwrap().unwrap();
    let e = book.find("alice", "mur", "origin").unwrap();
    assert_eq!(e.ref_prefix, "refs/heads/agent/alice/");
    assert!(e.creation_base_refs.is_empty());
    assert_eq!(
        files(&f).1.unwrap(),
        format!(
            "repos:\n  mur:\n    path: {}\n    remotes:\n    - origin\n",
            f.repo.display()
        )
    );
}

#[test]
fn agent_name_is_canonicalised_case_insensitively() {
    let f = fx();
    let (code, _) = go(&f, enroll_args(&f, "ALICE"));
    assert_eq!(code, 0);
    assert!(
        load(&f.home)
            .unwrap()
            .unwrap()
            .find("alice", "mur", "origin")
            .is_some()
    );
}

#[test]
fn re_enrolling_the_same_policy_changes_nothing() {
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    let before = files(&f);
    let (code, out) = go(&f, enroll_args(&f, "alice"));
    assert_eq!(code, 0);
    assert!(out.contains("nothing changed"), "{out}");
    assert_eq!(files(&f), before);
}

#[test]
fn a_changed_policy_updates_in_place_and_says_what_it_voids() {
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    let first = load(&f.home).unwrap().unwrap().enrollments[0].epoch;
    let GitPushAction::Enroll { .. } = enroll_args(&f, "alice") else {
        unreachable!()
    };
    let changed = GitPushAction::Enroll {
        agent: "alice".into(),
        repo: "mur".into(),
        remote: "origin".into(),
        endpoint: format!("file://{}", f.remote.display()),
        base_refs: vec!["refs/heads/main".into()],
        ref_prefix: None,
        path: None,
        object_format: None,
        auth: EnrollAuth::default(),
    };
    let (code, out) = go(&f, changed);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("policy_changed"), "{out}");
    assert!(load(&f.home).unwrap().unwrap().enrollments[0].epoch > first);
}

#[test]
fn a_short_base_ref_is_refused_with_a_hint_and_writes_nothing() {
    let f = fx();
    let GitPushAction::Enroll { .. } = enroll_args(&f, "alice") else {
        unreachable!()
    };
    let a = GitPushAction::Enroll {
        agent: "alice".into(),
        repo: "mur".into(),
        remote: "origin".into(),
        endpoint: format!("file://{}", f.remote.display()),
        base_refs: vec!["main".into()],
        ref_prefix: None,
        path: Some(f.repo.clone()),
        object_format: None,
        auth: EnrollAuth::default(),
    };
    let err = run_to(&f.home, a, &mut Vec::new()).unwrap_err().to_string();
    assert!(err.contains("refs/heads/main"), "{err}");
    assert_eq!(files(&f), (None, None));
}

#[test]
fn only_file_endpoints_are_accepted() {
    let f = fx();
    let GitPushAction::Enroll { .. } = enroll_args(&f, "alice") else {
        unreachable!()
    };
    let a = GitPushAction::Enroll {
        agent: "alice".into(),
        repo: "mur".into(),
        remote: "origin".into(),
        endpoint: "ssh://git@h/r.git".into(),
        base_refs: vec![],
        ref_prefix: None,
        path: Some(f.repo.clone()),
        object_format: None,
        auth: EnrollAuth::default(),
    };
    assert!(run_to(&f.home, a, &mut Vec::new()).is_err());
    assert_eq!(files(&f), (None, None));
}

#[test]
fn a_new_repo_without_a_path_is_explained() {
    let f = fx();
    let a = GitPushAction::Enroll {
        agent: "alice".into(),
        repo: "mur".into(),
        remote: "origin".into(),
        endpoint: format!("file://{}", f.remote.display()),
        base_refs: vec![],
        ref_prefix: None,
        path: None,
        object_format: None,
        auth: EnrollAuth::default(),
    };
    let err = run_to(&f.home, a, &mut Vec::new()).unwrap_err().to_string();
    assert!(err.contains("--path"), "{err}");
}

#[test]
fn a_different_path_for_a_known_repo_points_at_repo_move() {
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    let a = GitPushAction::Enroll {
        agent: "bob".into(),
        repo: "mur".into(),
        remote: "origin".into(),
        endpoint: format!("file://{}", f.remote.display()),
        base_refs: vec![],
        ref_prefix: None,
        path: Some(f.home.join("elsewhere")),
        object_format: None,
        auth: EnrollAuth::default(),
    };
    let err = run_to(&f.home, a, &mut Vec::new()).unwrap_err().to_string();
    assert!(err.contains("repo move"), "{err}");
}

#[test]
fn an_agent_that_can_write_the_remote_is_refused_and_nothing_is_written() {
    let f = fx();
    let mut p = AgentProfile::load(&f.home, "bob").unwrap();
    p.entitlements.filesystem.write = vec![f.remote.display().to_string()];
    std::fs::write(
        f.home.join("agents/bob/profile.yaml"),
        serde_yaml_ng::to_string(&p).unwrap(),
    )
    .unwrap();
    let (code, out) = go(&f, enroll_args(&f, "alice"));
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("bob can write"), "{out}");
    assert_eq!(files(&f), (None, None));
}

#[test]
fn an_agent_that_may_skip_its_sandbox_blocks_enrollment_for_everyone() {
    let f = fx();
    write_profile(&f.home, "dev", false);
    let (code, out) = go(&f, enroll_args(&f, "alice"));
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("agent dev may run without its sandbox"),
        "{out}"
    );
    assert_eq!(files(&f), (None, None));
}

#[test]
fn revoking_the_last_enrollment_removes_the_repo_and_the_registry_entry() {
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    let (code, _) = go(
        &f,
        GitPushAction::Revoke {
            agent: "alice".into(),
            repo: "mur".into(),
            remote: "origin".into(),
        },
    );
    assert_eq!(code, 0);
    assert!(load(&f.home).unwrap().unwrap().repos.is_empty());
    assert_eq!(files(&f).1.unwrap(), "repos: {}\n");
    let (code, _) = go(
        &f,
        GitPushAction::Revoke {
            agent: "alice".into(),
            repo: "mur".into(),
            remote: "origin".into(),
        },
    );
    assert_eq!(code, 1, "revoking twice reports it");
}

#[test]
fn re_enrolling_after_a_revoke_never_gets_the_old_epoch_back() {
    // The seam the broker's `re_enrolled_with_a_new_epoch_voids_a_pending_request`
    // relies on: revoke, then enroll the identical policy. The digest is
    // unchanged, so only a strictly larger epoch voids a stale pending row.
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    let first = load(&f.home).unwrap().unwrap().enrollments[0].epoch;
    let revoke = GitPushAction::Revoke {
        agent: "alice".into(),
        repo: "mur".into(),
        remote: "origin".into(),
    };
    assert_eq!(go(&f, revoke).0, 0);
    assert!(load(&f.home).unwrap().unwrap().enrollments.is_empty());
    assert_eq!(go(&f, enroll_args(&f, "alice")).0, 0);
    let again = load(&f.home).unwrap().unwrap().enrollments[0].epoch;
    assert!(
        again > first,
        "epoch went {first} -> {again}; it must only grow"
    );
}

#[test]
fn list_prints_json() {
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    let (_, out) = go(&f, GitPushAction::List { json: true });
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v[0]["agent"], "alice");
    assert_eq!(v[0]["ref_prefix"], "refs/heads/agent/alice/");
}

#[test]
fn doctor_flags_a_stale_registry_and_fix_repairs_it() {
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    std::fs::write(registry_path(&f.home), "repos: {}\n").unwrap();
    let (code, out) = go(
        &f,
        GitPushAction::Doctor {
            json: false,
            fix: false,
        },
    );
    assert_eq!(code, 0, "stale registry is a warning: {out}");
    assert!(out.contains("registry.yaml differs"), "{out}");
    let (_, out) = go(
        &f,
        GitPushAction::Doctor {
            json: false,
            fix: true,
        },
    );
    assert!(out.contains("regenerated"), "{out}");
    let want = load(&f.home).unwrap().unwrap().registry_yaml().unwrap();
    assert_eq!(files(&f).1.unwrap(), want);
}

#[test]
fn doctor_exits_nonzero_when_an_enrolled_agent_later_loses_its_sandbox() {
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    write_profile(&f.home, "alice", false);
    let (code, out) = go(
        &f,
        GitPushAction::Doctor {
            json: false,
            fix: false,
        },
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("alice may run without its sandbox"), "{out}");
}

fn plant_router(home: &Path) {
    mur_common::identity::AgentIdentity::generate()
        .save(
            &home
                .join("agents")
                .join(crate::channel_writer::ROUTER_AGENT),
        )
        .unwrap();
}

fn doctor_out(f: &Fx) -> (i32, String) {
    go(
        f,
        GitPushAction::Doctor {
            json: false,
            fix: false,
        },
    )
}

#[test]
fn doctor_refuses_when_enrolled_but_the_router_identity_is_missing() {
    let f = fx();
    for d in ["agents", "keys"] {
        std::fs::remove_dir_all(f.home.join(d).join(crate::channel_writer::ROUTER_AGENT)).unwrap();
    }
    go(&f, enroll_args(&f, "alice"));
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("no readable `mur` identity"), "{out}");
    plant_router(&f.home);
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("identity"), "{out}");
}

#[test]
fn doctor_does_not_ask_for_a_router_when_nothing_is_enrolled() {
    let f = fx();
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 0, "{out}");
}

fn set_git_push(f: &Fx, yaml: &str) {
    std::fs::write(f.home.join("config.yaml"), yaml).unwrap();
}

#[test]
fn doctor_warns_when_the_broker_is_on_but_no_agent_is_allowlisted() {
    let f = fx();
    set_git_push(&f, "git_push:\n  enabled: true\n  agents: []\n");
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 0, "a warning never fails the doctor: {out}");
    assert!(out.contains("git_push.agents is empty"), "{out}");
}

#[test]
fn doctor_warns_about_an_allowlisted_agent_that_does_not_exist() {
    let f = fx();
    set_git_push(&f, "git_push:\n  enabled: true\n  agents: [alice, ghost]\n");
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("ghost"), "{out}");
    assert!(out.contains("names an agent that does not exist"), "{out}");
    assert!(
        !out.contains("alice"),
        "an existing agent is not flagged: {out}"
    );
    assert!(!out.contains("agents is empty"), "{out}");
}

#[test]
fn doctor_says_nothing_about_the_allowlist_while_the_broker_is_off() {
    let f = fx();
    set_git_push(&f, "git_push:\n  enabled: false\n  agents: [ghost]\n");
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "ok", "{out}");
}

#[test]
fn doctor_is_quiet_when_the_allowlist_names_only_existing_agents() {
    let f = fx();
    set_git_push(&f, "git_push:\n  enabled: true\n  agents: [alice]\n");
    let (code, out) = doctor_out(&f);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.trim(), "ok", "{out}");
}

#[test]
fn a_corrupt_enrollment_file_stops_every_verb() {
    let f = fx();
    go(&f, enroll_args(&f, "alice"));
    std::fs::write(enrollments_path(&f.home), "version: 1\nnonsense: true\n").unwrap();
    assert!(
        run_to(
            &f.home,
            GitPushAction::List { json: false },
            &mut Vec::new()
        )
        .is_err()
    );
    assert!(run_to(&f.home, enroll_args(&f, "bob"), &mut Vec::new()).is_err());
}

#[test]
fn concurrent_enrolls_do_not_lose_a_record() {
    let f = fx();
    let home = f.home.clone();
    let remote = f.remote.clone();
    let repo = f.repo.clone();
    let handles: Vec<_> = ["alice", "bob"]
        .into_iter()
        .map(|who| {
            let (home, remote, repo) = (home.clone(), remote.clone(), repo.clone());
            std::thread::spawn(move || {
                run_to(
                    &home,
                    GitPushAction::Enroll {
                        agent: who.into(),
                        repo: "mur".into(),
                        remote: "origin".into(),
                        endpoint: format!("file://{}", remote.display()),
                        base_refs: vec![],
                        ref_prefix: None,
                        path: Some(repo),
                        object_format: None,
                        auth: EnrollAuth::default(),
                    },
                    &mut Vec::new(),
                )
                .unwrap()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 0);
    }
    let book = load(&f.home).unwrap().unwrap();
    assert_eq!(book.enrollments.len(), 2);
    assert_ne!(
        book.enrollments[0].epoch, book.enrollments[1].epoch,
        "the lock must also keep two concurrent enrolls from minting the same epoch"
    );
}

const HTTPS: &str = "https://git.example.com/o/r.git";

fn https_args(f: &Fx, auth: EnrollAuth) -> GitPushAction {
    GitPushAction::Enroll {
        agent: "alice".into(),
        repo: "mur".into(),
        remote: "origin".into(),
        endpoint: HTTPS.into(),
        base_refs: vec![],
        ref_prefix: None,
        path: Some(f.repo.clone()),
        object_format: None,
        auth,
    }
}

/// A token file the broker would accept: 0600, owned by us, under `<mur_home>/secrets/`.
fn plant_secret(f: &Fx, mode: u32) -> String {
    use std::os::unix::fs::PermissionsExt;
    let dir = f.home.join(SECRETS_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("git-token");
    std::fs::write(&p, "ghp_example_token\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    p.display().to_string()
}

fn with_credential(secret: String) -> EnrollAuth {
    EnrollAuth {
        username: Some("x-access-token".into()),
        secret: Some(secret),
        ..EnrollAuth::default()
    }
}

#[test]
fn an_https_endpoint_without_a_credential_is_refused_and_says_how_to_fix_it() {
    let f = fx();
    let (code, out) = go(&f, https_args(&f, EnrollAuth::default()));
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("--username and --secret"), "{out}");
    assert!(out.contains("--allow-unauthenticated"), "{out}");
    assert!(out.contains("nothing was written"), "{out}");
    assert_eq!(files(&f), (None, None));
}

#[test]
fn allow_unauthenticated_enrolls_an_https_endpoint_with_no_credential() {
    let f = fx();
    let auth = EnrollAuth {
        allow_unauthenticated: true,
        ..EnrollAuth::default()
    };
    let (code, out) = go(&f, https_args(&f, auth));
    assert_eq!(code, 0, "{out}");
    assert!(
        load(&f.home).unwrap().unwrap().enrollments[0]
            .credential
            .is_none()
    );
    let (_, list) = go(&f, GitPushAction::List { json: false });
    assert!(list.contains("unauthenticated"), "{list}");
}

#[test]
fn a_credential_is_written_to_the_enrollment_and_listed_without_the_token() {
    let f = fx();
    let secret = plant_secret(&f, 0o600);
    let (code, out) = go(&f, https_args(&f, with_credential(secret.clone())));
    assert_eq!(code, 0, "{out}");
    let cred = load(&f.home).unwrap().unwrap().enrollments[0]
        .credential
        .clone()
        .expect("credential stored");
    assert_eq!(cred.username.as_str(), "x-access-token");
    assert_eq!(cred.secret_path.as_str(), secret);

    let (_, json) = go(&f, GitPushAction::List { json: true });
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v[0]["credential"]["username"], "x-access-token");
    assert_eq!(v[0]["credential"]["secret_path"], secret.as_str());
    let (_, text) = go(&f, GitPushAction::List { json: false });
    assert!(text.contains("as x-access-token"), "{text}");
    for out in [&json, &text] {
        assert!(!out.contains("ghp_example_token"), "token leaked: {out}");
    }
}

#[test]
fn a_secret_file_the_broker_would_not_open_is_refused_and_nothing_is_written() {
    let f = fx();
    let secret = plant_secret(&f, 0o644);
    let (code, out) = go(&f, https_args(&f, with_credential(secret)));
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("bad_mode"), "{out}");
    assert!(out.contains("nothing was written"), "{out}");
    assert_eq!(files(&f), (None, None));
}

#[test]
fn a_malformed_username_or_secret_path_is_refused_before_any_check() {
    let f = fx();
    for (user, secret, needle) in [
        ("has space", "/abs/t", "--username"),
        ("u", "relative/t", "--secret"),
        ("u", "~/.mur/secrets/t", "--secret"),
    ] {
        let auth = EnrollAuth {
            username: Some(user.into()),
            secret: Some(secret.into()),
            ..EnrollAuth::default()
        };
        let (code, out) = go(&f, https_args(&f, auth));
        assert_eq!(code, 1, "{out}");
        assert!(out.contains(needle), "{needle}: {out}");
        assert_eq!(files(&f), (None, None));
    }
}

#[test]
fn https_only_flags_on_a_file_endpoint_are_refused() {
    let f = fx();
    let GitPushAction::Enroll { .. } = enroll_args(&f, "alice") else {
        unreachable!()
    };
    for auth in [
        EnrollAuth {
            allow_unauthenticated: true,
            ..EnrollAuth::default()
        },
        with_credential("/abs/t".into()),
    ] {
        let mut a = enroll_args(&f, "alice");
        if let GitPushAction::Enroll { auth: slot, .. } = &mut a {
            *slot = auth;
        }
        let (code, out) = go(&f, a);
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("only apply to an https://"), "{out}");
    }
    assert_eq!(files(&f), (None, None));
}

#[derive(clap::Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    action: GitPushAction,
}

#[test]
fn the_command_line_pairs_username_with_secret_and_keeps_them_apart_from_the_opt_in() {
    use clap::Parser;
    let base = [
        "mur",
        "enroll",
        "--agent",
        "a",
        "--repo",
        "r",
        "--remote",
        "o",
        "--endpoint",
        HTTPS,
    ];
    let parse = |extra: &[&str]| Cli::try_parse_from(base.iter().chain(extra));
    assert!(parse(&["--username", "u"]).is_err());
    assert!(parse(&["--secret", "/s"]).is_err());
    assert!(
        parse(&[
            "--allow-unauthenticated",
            "--username",
            "u",
            "--secret",
            "/s"
        ])
        .is_err()
    );
    assert!(parse(&["--username", "u", "--secret", "/s"]).is_ok());
    assert!(parse(&["--allow-unauthenticated"]).is_ok());
}

#[path = "git_push/read_tests.rs"]
mod read_tests;
