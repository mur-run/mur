use super::*;
use crate::tools::ToolExecutor;
use mur_common::identity::AgentIdentity;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

fn g(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().into()
}

struct Fx {
    t: tempfile::TempDir,
    inbox: PathBuf,
    repo: PathBuf,
    old: String,
    new: String,
    identity: Arc<AgentIdentity>,
    tool: GitPushRequestTool,
}

const REGISTRY: &str = "registry.yaml";

fn write_registry(root: &Path, repo: &Path) -> PathBuf {
    let p = root.join(REGISTRY);
    let other = root.join("elsewhere");
    std::fs::write(
        &p,
        format!(
            "repos:\n  repo:\n    path: {}\n    remotes: [origin]\n  other:\n    path: {}\n    remotes: [origin]\n",
            repo.display(),
            other.display()
        ),
    )
    .unwrap();
    p
}

fn fx() -> Fx {
    let t = tempfile::tempdir().unwrap();
    let repo = t.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    g(&repo, &["init", "-q", "-b", "main"]);
    g(&repo, &["commit", "-q", "--allow-empty", "-m", "old"]);
    let old = g(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    g(&repo, &["add", "f"]);
    g(&repo, &["commit", "-q", "-m", "new"]);
    let new = g(&repo, &["rev-parse", "HEAD"]);
    let inbox = t.path().join("agents/bob/inbox/git-push");
    let identity = Arc::new(AgentIdentity::generate());
    let tool = GitPushRequestTool::new(GitPushCtx {
        agent: "bob".into(),
        task_id: Some("t1".into()),
        inbox: inbox.clone(),
        registry_path: write_registry(t.path(), &repo),
        identity: identity.clone(),
        key_version: 0,
        now: Arc::new(chrono::Utc::now),
    });
    Fx {
        t,
        inbox,
        repo,
        old,
        new,
        identity,
        tool,
    }
}

fn args(f: &Fx) -> serde_json::Value {
    json!({"request_id":"r1","repo_id":"repo","remote_id":"origin","ref":"refs/heads/agent/x","old_sha":f.old,"new_sha":f.new})
}

fn files(d: &Path) -> Vec<String> {
    let mut v: Vec<_> = std::fs::read_dir(d)
        .map(|r| {
            r.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

async fn err(f: &Fx, a: serde_json::Value) -> String {
    format!("{:?}", f.tool.execute(a).await.expect_err("must reject"))
}

#[tokio::test]
async fn valid_request_writes_pack_and_signed_request() {
    let f = fx();
    let out = f.tool.execute(args(&f)).await.unwrap();
    assert!(
        out.text.contains("r1") && out.text.contains("pending"),
        "{}",
        out.text
    );
    assert_eq!(files(&f.inbox), ["r1.pack", "r1.yaml"]);
    let r = mur_git_broker::request_file::verify_request(
        &f.inbox.join("r1.yaml"),
        "bob",
        &f.identity.verifying_key_bytes(),
    )
    .unwrap();
    assert_eq!(
        (r.task_id.as_str(), r.old_sha.as_str()),
        ("t1", f.old.as_str())
    );
    assert!(r.verify_pack(&f.inbox.join("r1.pack")));
}

/// The pack is `new ^old`: it carries the new commit and its tree/blob, not `old`.
#[tokio::test]
async fn pack_covers_the_range_and_indexes_cleanly() {
    let f = fx();
    f.tool.execute(args(&f)).await.unwrap();
    // verify-pack finds the .pack beside the .idx, so index a copy outside the inbox.
    let pack = f.t.path().join("r1.pack");
    std::fs::copy(f.inbox.join("r1.pack"), &pack).unwrap();
    let idx = f.t.path().join("r1.idx");
    g(
        &f.repo,
        &[
            "index-pack",
            "-o",
            idx.to_str().unwrap(),
            pack.to_str().unwrap(),
        ],
    );
    let listing = g(&f.repo, &["verify-pack", "-v", idx.to_str().unwrap()]);
    assert!(listing.contains(&f.new), "{listing}");
    assert!(
        !listing.contains(&f.old),
        "old is prefetched by the broker, not packed: {listing}"
    );
}

#[tokio::test]
async fn request_rejects_null_missing_and_empty_old_sha() {
    let f = fx();
    for bad in [json!(null), json!(""), json!("0"), json!("null")] {
        let mut a = args(&f);
        a["old_sha"] = bad.clone();
        assert!(err(&f, a).await.contains("invalid_request"), "{bad}");
    }
    let mut a = args(&f);
    a.as_object_mut().unwrap().remove("old_sha");
    assert!(err(&f, a).await.contains("invalid_request"));
    assert!(
        files(&f.inbox).is_empty(),
        "nothing written for a rejected request"
    );
}

#[tokio::test]
async fn creation_uses_the_all_zero_oid_only() {
    let f = fx();
    let mut a = args(&f);
    a["old_sha"] = json!("0".repeat(40));
    a["ref"] = json!("refs/heads/agent/fresh");
    f.tool.execute(a).await.unwrap();
    let mut d = args(&f);
    d["request_id"] = json!("r2");
    d["new_sha"] = json!("0".repeat(40));
    assert!(
        err(&f, d).await.contains("invalid_request"),
        "a zero new_sha is a delete"
    );
}

#[tokio::test]
async fn request_rejects_non_agent_refs_tags_and_short_oids() {
    let f = fx();
    let upper = "A".repeat(40);
    for (k, v) in [
        ("ref", "refs/heads/main"),
        ("ref", "refs/tags/v1"),
        ("ref", "main"),
        ("ref", "refs/heads/agent/../main"),
        ("new_sha", "abc123"),
        ("new_sha", upper.as_str()),
        ("old_sha", "abc123"),
    ] {
        let mut a = args(&f);
        a[k] = json!(v);
        assert!(err(&f, a).await.contains("invalid_request"), "{k}={v}");
    }
}

#[tokio::test]
async fn request_never_accepts_a_path_url_or_git_option() {
    let f = fx();
    for (k, v) in [
        ("path", "/tmp/x"),
        ("url", "https://evil"),
        ("remote_url", "https://evil"),
        ("force", "true"),
        ("options", "--mirror"),
        ("cwd", "/"),
    ] {
        let mut a = args(&f);
        a[k] = json!(v);
        assert!(
            err(&f, a).await.contains("invalid_request"),
            "extra key {k} must be refused"
        );
    }
    for (k, v) in [
        ("repo_id", "/etc"),
        ("repo_id", "unknown"),
        ("remote_id", "not-enrolled"),
    ] {
        let mut a = args(&f);
        a[k] = json!(v);
        assert!(err(&f, a).await.contains("invalid_request"), "{k}={v}");
    }
}

/// Premise 2: no rejection names a path or another enrolled repo.
#[tokio::test]
async fn rejections_never_echo_the_registry_mapping() {
    let f = fx();
    let root = f.t.path().to_string_lossy().into_owned();
    let mut probes = Vec::new();
    for (k, v) in [
        ("repo_id", "unknown"),
        ("repo_id", "other"),
        ("remote_id", "nope"),
    ] {
        let mut a = args(&f);
        a[k] = json!(v);
        probes.push(a);
    }
    let mut missing = args(&f);
    missing["new_sha"] = json!("e".repeat(40));
    probes.push(missing);
    for a in probes {
        let e = err(&f, a.clone()).await;
        assert!(!e.contains(&root), "{a}: leaked a path: {e}");
        assert!(
            !e.contains("elsewhere") && !e.contains("other\""),
            "{a}: leaked the mapping: {e}"
        );
    }
}

#[tokio::test]
async fn request_with_an_oid_the_repo_lacks_is_rejected_before_writing() {
    let f = fx();
    let mut a = args(&f);
    a["new_sha"] = json!("e".repeat(40));
    assert!(err(&f, a).await.contains("invalid_request"));
    assert!(files(&f.inbox).is_empty());
}

#[tokio::test]
async fn same_request_id_is_idempotent_on_the_agent_side() {
    let f = fx();
    f.tool.execute(args(&f)).await.unwrap();
    let first = std::fs::read(f.inbox.join("r1.yaml")).unwrap();
    f.tool.execute(args(&f)).await.unwrap();
    assert_eq!(files(&f.inbox), ["r1.pack", "r1.yaml"]);
    assert_eq!(first, std::fs::read(f.inbox.join("r1.yaml")).unwrap());
    let mut b = args(&f);
    b["new_sha"] = json!(f.old.clone());
    assert!(err(&f, b).await.contains("request_conflict"));
}

#[test]
fn tool_defs_expose_exactly_the_spec_arguments() {
    let f = fx();
    let d = f.tool.def();
    assert_eq!(d.name, "git_push_request");
    let mut props: Vec<_> = d.input_schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    props.sort();
    assert_eq!(
        props,
        [
            "new_sha",
            "old_sha",
            "ref",
            "remote_id",
            "repo_id",
            "request_id"
        ]
    );
    assert_eq!(d.input_schema["additionalProperties"], json!(false));
    let mut req: Vec<_> = d.input_schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    req.sort();
    assert_eq!(req, props, "every argument is required — including old_sha");
}

#[tokio::test]
async fn identity_task_and_epoch_cannot_be_supplied_by_the_model() {
    let f = fx();
    for (k, v) in [
        ("agent", json!("alice")),
        ("task_id", json!("t9")),
        ("enrollment_epoch", json!(7)),
    ] {
        let mut a = args(&f);
        a[k] = v;
        assert!(err(&f, a).await.contains("invalid_request"), "{k}");
    }
}

/// Production passes no fixed task id: it comes from the runtime's turn scope,
/// and a call outside any turn is refused rather than signed with a blank id.
#[tokio::test]
async fn task_id_comes_from_the_runtime_scope() {
    let mut f = fx();
    f.tool.ctx.task_id = None;
    assert!(f.tool.execute(args(&f)).await.is_err(), "no task scope");
    assert!(files(&f.inbox).is_empty());
    crate::tools::bash_jobs::CURRENT_TASK_ID
        .scope("task-42".into(), f.tool.execute(args(&f)))
        .await
        .unwrap();
    let r = mur_git_broker::request_file::read_request(&f.inbox.join("r1.yaml")).unwrap();
    assert_eq!(r.task_id, "task-42");
}

/// The registry is re-read per call: a repo the daemon removes is gone at once,
/// and an unparseable registry is an error, not "nothing enrolled".
#[tokio::test]
async fn registry_is_reread_and_a_corrupt_one_fails_closed() {
    let f = fx();
    let reg = f.t.path().join(REGISTRY);
    std::fs::write(&reg, "repos: {}\n").unwrap();
    assert!(err(&f, args(&f)).await.contains("unknown repo_id"));
    std::fs::write(&reg, "repos: [not, a, map]\n").unwrap();
    assert!(err(&f, args(&f)).await.contains("registry unreadable"));
    std::fs::remove_file(&reg).unwrap();
    assert!(err(&f, args(&f)).await.contains("unknown repo_id"));
}

// ---- status / cancel ------------------------------------------------------------

#[tokio::test]
async fn status_reads_only_the_broker_status_file() {
    let f = fx();
    let dir = f.t.path().join("status");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("r1.yaml"), "state: pending_approval\ncode: null\n").unwrap();
    let t = GitPushStatusTool::new(dir);
    let out = t.execute(json!({"request_id":"r1"})).await.unwrap();
    assert!(out.text.contains("pending_approval"));
    assert!(
        t.execute(json!({"request_id":"../../etc/passwd"}))
            .await
            .is_err(),
        "ids are names, not paths"
    );
    assert!(
        t.execute(json!({"request_id":"nope"}))
            .await
            .unwrap()
            .text
            .contains("unknown")
    );
}

#[tokio::test]
async fn cancel_drops_a_cancel_marker_and_reports_pending_cancel() {
    let f = fx();
    let dir = f.t.path().join("cancel");
    let t = GitPushCancelTool::new(dir.clone());
    let out = t.execute(json!({"request_id":"r1"})).await.unwrap();
    assert!(
        out.text.contains("pending_cancel") && !out.text.contains("cancelled"),
        "{}",
        out.text
    );
    assert_eq!(files(&dir), ["r1.cancel"]);
    t.execute(json!({"request_id":"r1"})).await.unwrap();
    assert_eq!(files(&dir), ["r1.cancel"], "idempotent");
    assert!(t.execute(json!({"request_id":"../x"})).await.is_err());
}
