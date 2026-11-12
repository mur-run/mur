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
fn a_recorded_verdict_spends_the_run_id() {
    let (tmp, id) = home("");
    let r = req(&id, "coder");
    ensure_unspent(tmp.path(), &r).unwrap();
    let dir = run_dir(tmp.path(), &r);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(JOBS_DISPATCH_RECORD_FILE), "{}").unwrap();
    assert!(ensure_unspent(tmp.path(), &r).is_err());
}
