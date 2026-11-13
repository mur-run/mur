use super::*;
use chrono::Utc;
use mur_track::jobs_dispatch_request::{JobsDispatch, RequestedJob};

fn home(config: &str) -> (tempfile::TempDir, AgentIdentity) {
    let tmp = tempfile::tempdir().unwrap();
    for a in ["pm", "coder"] {
        let dir = tmp.path().join("agents").join(a);
        std::fs::create_dir_all(&dir).unwrap();
        // Name resolution only counts a directory with a profile.
        std::fs::write(dir.join("profile.yaml"), format!("name: {a}\n")).unwrap();
    }
    let id = AgentIdentity::generate();
    id.save(&tmp.path().join("agents/pm")).unwrap();
    std::fs::write(tmp.path().join("config.yaml"), config).unwrap();
    (tmp, id)
}

fn req(id: &AgentIdentity, agent: &str) -> JobsDispatchRequest {
    JobsDispatchRequest::create(
        JobsDispatch {
            requested_by: "pm".into(),
            jobs: vec![RequestedJob {
                description: "x".into(),
                agent: agent.into(),
            }],
            max_concurrency: 1,
            cwd: None,
            cwd_inferred: true,
            run_id: format!("run-{}", uuid::Uuid::now_v7()),
            channel_id: uuid::Uuid::now_v7().to_string(),
        },
        id,
        0,
        Utc::now(),
    )
}

fn cfg(home: &Path) -> Config {
    Config::try_load(&home.join("config.yaml")).unwrap()
}

#[test]
fn a_valid_request_verifies_and_authorizes() {
    let (tmp, id) = home("parallel_jobs:\n  targets: [coder]\n");
    let r = req(&id, "coder");
    verify_request(tmp.path(), &r, Some(60)).unwrap();
    authorize(tmp.path(), &r, &cfg(tmp.path())).unwrap();
}

#[test]
fn the_child_check_skips_freshness_but_not_the_signature() {
    let (tmp, id) = home("parallel_jobs:\n  targets: [coder]\n");
    let mut r = req(&id, "coder");
    r.requested_at -= chrono::Duration::hours(2);
    // Re-sign at the old time so only the age is wrong.
    r = JobsDispatchRequest::create(r.dispatch, &id, 0, r.requested_at);
    assert!(verify_request(tmp.path(), &r, Some(60)).is_err());
    verify_request(tmp.path(), &r, None).unwrap();
    r.dispatch.jobs[0].agent = "root".into();
    assert!(verify_request(tmp.path(), &r, None).is_err());
}

#[test]
fn requesters_empty_means_anyone_listed_means_only_them() {
    let (tmp, id) = home("parallel_jobs:\n  targets: [coder]\n  requesters: [mur]\n");
    let err = authorize(tmp.path(), &req(&id, "coder"), &cfg(tmp.path())).unwrap_err();
    assert!(err.to_string().contains("requesters"), "{err}");
    // Case-insensitive like every other agent lookup (CLAUDE.md rule 10).
    std::fs::write(
        tmp.path().join("config.yaml"),
        "parallel_jobs:\n  targets: [coder]\n  requesters: [PM]\n",
    )
    .unwrap();
    authorize(tmp.path(), &req(&id, "coder"), &cfg(tmp.path())).unwrap();
}

#[test]
fn self_delegation_is_refused_even_when_allowlisted() {
    let (tmp, id) = home("parallel_jobs:\n  targets: [coder, pm]\n");
    let err = authorize(tmp.path(), &req(&id, "pm"), &cfg(tmp.path())).unwrap_err();
    assert!(err.to_string().contains("itself"), "{err}");
}

#[test]
fn spending_a_run_id_is_once_only() {
    let (tmp, id) = home("");
    let r = req(&id, "coder");
    let dir = spend_run_id(tmp.path(), &r).unwrap();
    assert_eq!(dir, jobs_dispatch_run_dir(tmp.path(), &r.dispatch.run_id));
    assert!(dir.is_dir());
    let err = spend_run_id(tmp.path(), &r).unwrap_err();
    assert!(err.to_string().contains("replay"), "{err}");
}

/// Even a planted link at the run's path (the tree is write-denied to agents,
/// so this is defence in depth) reads as spent, never as a place to write.
#[cfg(unix)]
#[test]
fn a_link_at_the_run_path_is_never_written_through() {
    let (tmp, id) = home("");
    let r = req(&id, "coder");
    let victim = tmp.path().join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    let dir = jobs_dispatch_run_dir(tmp.path(), &r.dispatch.run_id);
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, &dir).unwrap();
    assert!(spend_run_id(tmp.path(), &r).is_err());
}

/// Review P3 (#2386): the inbox is agent-writable, so a symlink, a FIFO or an
/// oversized file is refused instead of read — and a FIFO never blocks.
#[test]
fn read_request_refuses_links_fifos_and_oversized_files() {
    let (tmp, id) = home("");
    let inbox = tmp.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let good = inbox.join("good.yaml");
    std::fs::write(&good, serde_yaml_ng::to_string(&req(&id, "coder")).unwrap()).unwrap();
    assert!(read_request(&good).is_ok());

    let big = inbox.join("big.yaml");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(mur_track::jobs_dispatch_request::MAX_REQUEST_BYTES + 1)
        .unwrap();
    assert!(read_request(&big).is_err());

    #[cfg(unix)]
    {
        let link = inbox.join("link.yaml");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        assert!(read_request(&link).is_err(), "symlink must not be followed");

        let fifo = inbox.join("fifo.yaml");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        // No writer: a blocking open would hang this test forever.
        assert!(read_request(&fifo).is_err(), "FIFO must be refused");
    }
}
