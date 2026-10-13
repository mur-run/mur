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
    assert_eq!(load(&f.home).unwrap().unwrap().enrollments.len(), 2);
}
