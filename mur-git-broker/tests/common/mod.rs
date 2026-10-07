#![allow(dead_code)]
use mur_git_broker::{action::*, constants::*, oid::ObjectFormat, policy::RemotePolicy};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Absolute path of the system git. The runner clears the environment, so it needs an absolute
/// binary path (a bare "git" would be resolved against an empty PATH and fail to spawn).
pub fn git_bin() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("git"))
        .find(|c| c.is_file())
        .expect("git on PATH")
}
pub fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("spawn git");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}
pub fn commit(dir: &Path, msg: &str) -> String {
    git(dir, &["commit", "-q", "--allow-empty", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}
pub fn work_repo(root: &Path) -> PathBuf {
    let p = root.join("work");
    std::fs::create_dir_all(&p).unwrap();
    git(&p, &["init", "-q", "-b", "main"]);
    p
}
pub fn bare_remote(root: &Path) -> PathBuf {
    let p = root.join("remote.git");
    git(root, &["init", "-q", "--bare", p.to_str().unwrap()]);
    p
}
/// `git pack-objects --revs --stdout` for `new ^old` (old = None ⇒ everything reachable from new).
pub fn range_pack(work: &Path, new: &str, old: Option<&str>, thin: bool) -> PathBuf {
    let revs = match old {
        Some(o) => format!("{new}\n^{o}\n"),
        None => format!("{new}\n"),
    };
    let out = work.join(format!("range-{}.pack", &new[..8]));
    let mut a = vec!["pack-objects", "--revs", "--stdout"];
    if thin {
        a.push("--thin");
    }
    let mut c = Command::new("git")
        .current_dir(work)
        .args(a)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    c.stdin.take().unwrap().write_all(revs.as_bytes()).unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success());
    std::fs::write(&out, o.stdout).unwrap();
    out
}
pub fn policy() -> RemotePolicy {
    RemotePolicy {
        remote_id: "origin".into(),
        canonical_remote_endpoint: "file:///remote.git".into(),
        creation_base_refs: vec!["refs/heads/main".into()],
        ref_prefix: ALLOWED_REF_PREFIX.into(),
    }
}
pub fn action_for(old: &str, new: &str, r#ref: &str) -> ActionDocument {
    let p = policy();
    ActionDocument {
        version: ACTION_VERSION.into(),
        agent_id: "alice".into(),
        task_id: "t1".into(),
        enrollment_epoch: 1,
        request_id: "r1".into(),
        repo_id: "repo".into(),
        repo_identity: "id".into(),
        object_format: ObjectFormat::Sha1,
        remote_id: p.remote_id.clone(),
        canonical_remote_endpoint: p.canonical_remote_endpoint.clone(),
        remote_policy_digest: "rpd".into(),
        ref_policy_digest: p.digest(),
        updates: vec![RefUpdate {
            r#ref: r#ref.into(),
            old_sha: old.into(),
            new_sha: new.into(),
        }],
        force: false,
        delete: false,
    }
}
