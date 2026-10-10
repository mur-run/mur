#![cfg(unix)]
use mur_git_broker::{constants::*, git::*};
use std::time::Duration;
mod common;
use common::*;
const T: Duration = Duration::from_secs(DEFAULT_GIT_TIMEOUT_SECS);

#[test]
fn inherited_git_env_cannot_forge_ancestry() {
    let t = tempfile::tempdir().unwrap();
    let w = work_repo(t.path());
    let a = commit(&w, "a");
    let b = {
        git(&w, &["checkout", "-q", "--orphan", "x"]);
        commit(&w, "b")
    };
    let graft = t.path().join("grafts");
    std::fs::write(&graft, format!("{b} {a}\n")).unwrap();
    // SAFETY: this test file must run with `--test-threads=1` (env-mutating tests).
    unsafe {
        std::env::set_var("GIT_GRAFT_FILE", &graft);
    }
    let out = GitRunner::new(git_bin(), w.join(".git"))
        .run(&["merge-base", "--is-ancestor", &a, &b], T)
        .unwrap();
    unsafe {
        std::env::remove_var("GIT_GRAFT_FILE");
    }
    assert_eq!(out.code, 1, "scrubbed env must ignore GIT_GRAFT_FILE");
}
#[test]
fn config_count_injection_is_ignored() {
    let t = tempfile::tempdir().unwrap();
    let w = work_repo(t.path());
    unsafe {
        std::env::set_var("GIT_CONFIG_COUNT", "1");
        std::env::set_var("GIT_CONFIG_KEY_0", "url.X.insteadOf");
        std::env::set_var("GIT_CONFIG_VALUE_0", "Y");
    }
    let out = GitRunner::new(git_bin(), w.join(".git"))
        .run(&["config", "--get", "url.X.insteadOf"], T)
        .unwrap();
    for k in ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"] {
        unsafe {
            std::env::remove_var(k);
        }
    }
    assert_eq!(out.code, 1, "key absent ⇒ exit 1");
    assert!(out.stdout.is_empty());
}
#[test]
fn hooks_never_run() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let w = work_repo(t.path());
    commit(&w, "c");
    let remote = bare_remote(t.path());
    let marker = t.path().join("MARKER");
    let hook = w.join(".git/hooks/pre-push");
    std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Guard against a vacuous pass: if this environment cannot exec the hook, "marker absent" proves nothing.
    let ran = std::process::Command::new(&hook)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && marker.exists();
    if !ran {
        // A "marker absent" check proves nothing when hooks cannot run here, so fail loudly
        // instead of reporting a silent pass. Opting out is explicit and visible.
        if hook_skip_allowed() {
            eprintln!("SKIP hooks_never_run ({ALLOW_HOOK_SKIP_ENV}=1): cannot exec hooks here");
            return;
        }
        panic!(
            "hooks_never_run cannot verify hook isolation: this environment cannot exec hook \
             scripts. Fix the environment, or set {ALLOW_HOOK_SKIP_ENV}=1 to skip explicitly."
        );
    }
    std::fs::remove_file(&marker).unwrap();
    git(
        &w,
        &[
            "config",
            "core.hooksPath",
            w.join(".git/hooks").to_str().unwrap(),
        ],
    );
    let out = GitRunner::new(git_bin(), w.join(".git"))
        .run(
            &[
                "push",
                "-q",
                remote.to_str().unwrap(),
                "HEAD:refs/heads/agent/x",
            ],
            T,
        )
        .unwrap();
    assert_eq!(out.code, 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        !marker.exists(),
        "pre-push hook ran despite -c core.hooksPath=/dev/null"
    );
}
#[test]
fn output_is_not_localised() {
    let t = tempfile::tempdir().unwrap();
    let w = work_repo(t.path());
    unsafe {
        std::env::set_var("LC_ALL", "zh_TW.UTF-8");
        std::env::set_var("LANG", "zh_TW.UTF-8");
    }
    let out = GitRunner::new(git_bin(), w.join(".git"))
        .run(&["rev-parse", "--verify", "no-such-ref"], T)
        .unwrap();
    for k in ["LC_ALL", "LANG"] {
        unsafe {
            std::env::remove_var(k);
        }
    }
    assert_eq!(out.code, 128);
    assert!(
        String::from_utf8_lossy(&out.stderr).is_ascii(),
        "stderr must be the C-locale text (localised text is non-ASCII)"
    );
}
#[test]
fn timeout_kills_and_reports() {
    let mut c = std::process::Command::new("/bin/sleep");
    c.arg("30");
    let start = std::time::Instant::now();
    assert!(matches!(
        run_with_timeout(c, Duration::from_millis(300)),
        Err(GitError::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_secs(5));
}
#[test]
fn signal_death_is_reported_not_swallowed() {
    let mut c = std::process::Command::new("/bin/sh");
    c.args(["-c", "kill -9 $$"]);
    assert!(matches!(run_with_timeout(c, T), Err(GitError::Signal)));
}
#[test]
fn large_output_does_not_deadlock_the_wait() {
    let mut c = std::process::Command::new("/bin/sh");
    c.args(["-c", "yes | head -c 5000000"]);
    assert_eq!(run_with_timeout(c, T).unwrap().stdout.len(), 5_000_000);
}
