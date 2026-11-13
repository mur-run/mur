//! Daemon half of #2379. The fake `mur` records its argv and `MUR_RUN_OWNER`
//! so the tests can see exactly what the daemon launched, and for whom.

use super::*;
use chrono::Utc;
use mur_common::identity::AgentIdentity;
use mur_track::jobs_dispatch_request::{JobsDispatch, RequestedJob};

struct Fixture {
    tmp: tempfile::TempDir,
    requester: AgentIdentity,
}

impl Fixture {
    fn home(&self) -> &Path {
        self.tmp.path()
    }
}

/// MUR home with agent `pm` (real identity, the requester), and a config
/// that lets `pm` delegate to `coder`. `extra` is appended to the config.
fn fixture(extra: &str) -> Fixture {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    for agent in ["pm", "coder"] {
        std::fs::create_dir_all(home.join("agents").join(agent)).unwrap();
    }
    let requester = AgentIdentity::generate();
    requester.save(&home.join("agents/pm")).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!("parallel_jobs:\n  targets: [coder]\n{extra}"),
    )
    .unwrap();
    Fixture { tmp, requester }
}

#[cfg(windows)]
const WORK: &str = r"C:\work";
#[cfg(not(windows))]
const WORK: &str = "/work";

fn dispatch(agent: &str) -> JobsDispatch {
    JobsDispatch {
        requested_by: "pm".into(),
        jobs: vec![RequestedJob {
            description: "fix it".into(),
            agent: agent.into(),
        }],
        max_concurrency: 2,
        cwd: Some(WORK.into()),
        cwd_inferred: false,
        run_id: format!("run-{}", uuid::Uuid::now_v7()),
        channel_id: uuid::Uuid::now_v7().to_string(),
    }
}

fn drop_request(home: &Path, req: &JobsDispatchRequest) -> PathBuf {
    let dir = home.join(JOBS_DISPATCH_REQUEST_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{}.yaml", req.dispatch.run_id));
    std::fs::write(&p, serde_yaml_ng::to_string(req).unwrap()).unwrap();
    p
}

#[cfg(unix)]
fn fake_mur(home: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let fake = home.join("fake-mur");
    let out = home.join("launched");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n{{ echo \"$@\"; echo \"owner=${}\"; echo \"hosted=${}\"; }} > {o}.tmp && mv {o}.tmp {o}\n",
            mur_common::paths::RUN_OWNER_ENV,
            JOBS_DISPATCH_HOSTED_ENV,
            o = out.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::process::Command::new(&fake)
        .status()
        .is_ok_and(|s| s.success())
        .then(|| {
            let _ = std::fs::remove_file(&out);
            fake
        })
}

#[cfg(unix)]
fn launched(home: &Path) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(s) = std::fs::read_to_string(home.join("launched")) {
            return Some(s);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn record(home: &Path, req: &JobsDispatchRequest) -> Option<JobsDispatchRecord> {
    let p = mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(home, &req.dispatch.run_id)
        .join(JOBS_DISPATCH_RECORD_FILE);
    std::fs::read(p)
        .ok()
        .map(|b| serde_json::from_slice(&b).unwrap())
}

fn cfg(home: &Path) -> Config {
    Config::try_load(&home.join("config.yaml")).unwrap()
}

fn sweep_once(home: &Path, bin: &Path) {
    sweep(home, &home.join(JOBS_DISPATCH_REQUEST_DIR), &cfg(home), bin).unwrap();
}

#[cfg(unix)]
#[test]
fn verified_request_launches_once_as_the_requester_and_records_it() {
    let f = fixture("");
    let Some(bin) = fake_mur(f.home()) else {
        eprintln!("skipped: sandbox forbids exec of a temp script");
        return;
    };
    let req = JobsDispatchRequest::create(dispatch("coder"), &f.requester, 0, Utc::now());
    drop_request(f.home(), &req);
    sweep_once(f.home(), &bin);

    let out = launched(f.home()).expect("child launched");
    assert!(out.contains("job run-request"), "{out}");
    let admitted =
        mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(f.home(), &req.dispatch.run_id)
            .join(JOBS_DISPATCH_ADMITTED_FILE);
    assert!(
        out.contains(&admitted.display().to_string()),
        "the child reads the daemon's admitted copy: {out}"
    );
    assert!(
        out.lines().any(|l| l == "owner=") && out.lines().any(|l| l == "hosted=1"),
        "the run is hosted in the daemon tree, not an agent home: {out:?}"
    );
    let rec = record(f.home(), &req).expect("verdict written");
    assert_eq!(rec.status, JobsDispatchStatus::Launched);
    assert_eq!(rec.dispatched_by, "pm");
    assert!(rec.pid.is_some());
    assert!(
        std::fs::read_dir(f.home().join(JOBS_DISPATCH_REQUEST_DIR))
            .unwrap()
            .next()
            .is_none(),
        "inbox must be drained"
    );
}

#[cfg(unix)]
#[test]
fn a_replayed_request_is_rejected_and_keeps_the_original_record() {
    let f = fixture("");
    let Some(bin) = fake_mur(f.home()) else {
        eprintln!("skipped: sandbox forbids exec of a temp script");
        return;
    };
    let req = JobsDispatchRequest::create(dispatch("coder"), &f.requester, 0, Utc::now());
    drop_request(f.home(), &req);
    sweep_once(f.home(), &bin);
    launched(f.home()).expect("first launch");
    let first = record(f.home(), &req).unwrap();
    std::fs::remove_file(f.home().join("launched")).unwrap();

    drop_request(f.home(), &req);
    sweep_once(f.home(), &bin);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !f.home().join("launched").exists(),
        "replay must not launch"
    );
    assert_eq!(record(f.home(), &req).unwrap(), first);
}

/// Every one of these is refused, and none of them launches. The ones that
/// fail before the signature verifies write no record at all: an unverified
/// file may not write anywhere.
#[test]
fn rejections_never_launch() {
    let f = fixture("");
    let bin = f.home().join("must-not-run");
    let stranger = AgentIdentity::generate();
    let now = Utc::now();
    let cases: Vec<(&str, JobsDispatchRequest, bool)> = vec![
        (
            "forged signature",
            JobsDispatchRequest::create(dispatch("coder"), &stranger, 0, now),
            false,
        ),
        (
            "stale",
            JobsDispatchRequest::create(
                dispatch("coder"),
                &f.requester,
                0,
                now - chrono::Duration::hours(1),
            ),
            false,
        ),
        (
            "relative cwd",
            JobsDispatchRequest::create(
                JobsDispatch {
                    cwd: Some("rel".into()),
                    ..dispatch("coder")
                },
                &f.requester,
                0,
                now,
            ),
            false,
        ),
        (
            "target not allowlisted",
            JobsDispatchRequest::create(dispatch("ghost"), &f.requester, 0, now),
            true,
        ),
        (
            "self-delegation",
            JobsDispatchRequest::create(dispatch("pm"), &f.requester, 0, now),
            true,
        ),
    ];
    for (why, req, verified) in cases {
        drop_request(f.home(), &req);
        sweep_once(f.home(), &bin);
        let rec = record(f.home(), &req);
        if verified {
            let rec = rec.unwrap_or_else(|| panic!("{why}: a verified rejection is recorded"));
            assert_eq!(rec.status, JobsDispatchStatus::Rejected, "{why}");
            assert!(rec.reason.is_some(), "{why}");
        } else {
            assert!(rec.is_none(), "{why}: unverified must write nothing");
        }
    }
}

#[test]
fn a_requester_outside_the_allowlist_is_refused() {
    // Q1: an explicit `requesters` list that does not name pm.
    let f = fixture("  requesters: [mur]\n");
    let bin = f.home().join("must-not-run");
    let req = JobsDispatchRequest::create(dispatch("coder"), &f.requester, 0, Utc::now());
    drop_request(f.home(), &req);
    sweep_once(f.home(), &bin);
    let rec = record(f.home(), &req).expect("recorded");
    assert_eq!(rec.status, JobsDispatchStatus::Rejected);
    assert!(rec.reason.unwrap().contains("parallel_jobs.requesters"));
}

#[tokio::test]
async fn a_rejected_dispatch_polls_as_failed_with_its_reason() {
    let f = fixture("");
    let bin = f.home().join("must-not-run");
    let req = JobsDispatchRequest::create(dispatch("ghost"), &f.requester, 0, Utc::now());
    drop_request(f.home(), &req);
    sweep_once(f.home(), &bin);
    let status = mur_core::run_status::status_of(f.home(), &req.dispatch.run_id)
        .unwrap()
        .expect("the handle must resolve");
    assert_eq!(status.state, mur_core::run_status::State::Failed);
    let err = status.run.steps[0].error.clone().unwrap();
    assert!(err.contains("parallel_jobs.targets"), "{err}");
}

/// Review P1 (#2386): the requester can write its own home, so nothing the
/// unsealed daemon writes may live there. A link the agent planted at its
/// old run path is never followed: the daemon writes only its own tree.
#[cfg(unix)]
#[test]
fn a_link_planted_in_the_requesters_home_is_never_written_through() {
    let f = fixture("");
    let Some(bin) = fake_mur(f.home()) else {
        eprintln!("skipped: sandbox forbids exec of a temp script");
        return;
    };
    let req = JobsDispatchRequest::create(dispatch("coder"), &f.requester, 0, Utc::now());
    let victim = f.home().join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join(JOBS_RUN_LOG_FILE), "precious").unwrap();
    let runs = mur_common::paths::agent_runs_dir(f.home(), "pm");
    std::fs::create_dir_all(&runs).unwrap();
    std::os::unix::fs::symlink(&victim, runs.join(&req.dispatch.run_id)).unwrap();

    drop_request(f.home(), &req);
    sweep_once(f.home(), &bin);
    launched(f.home()).expect("launched");
    assert_eq!(
        std::fs::read_to_string(victim.join(JOBS_RUN_LOG_FILE)).unwrap(),
        "precious"
    );
    assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
}

/// A link planted at the daemon's own run path (defence in depth: agents are
/// write-denied there) reads as a spent id; nothing is written or launched.
#[cfg(unix)]
#[test]
fn a_link_at_the_daemon_run_path_is_refused() {
    let f = fixture("");
    let bin = f.home().join("must-not-run");
    let req = JobsDispatchRequest::create(dispatch("coder"), &f.requester, 0, Utc::now());
    let victim = f.home().join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    let dir =
        mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(f.home(), &req.dispatch.run_id);
    std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&victim, &dir).unwrap();
    drop_request(f.home(), &req);
    sweep_once(f.home(), &bin);
    assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 0);
}

/// Review P2 (#2386): the inbox file is swapped for another signed request
/// after it was read. The launched child must get the ADMITTED bytes, and
/// the daemon must never reread the inbox path.
#[cfg(unix)]
#[test]
fn a_request_swapped_after_admission_is_never_executed() {
    let f = fixture("");
    let Some(bin) = fake_mur(f.home()) else {
        eprintln!("skipped: sandbox forbids exec of a temp script");
        return;
    };
    let admitted = JobsDispatchRequest::create(dispatch("coder"), &f.requester, 0, Utc::now());
    let path = drop_request(f.home(), &admitted);
    let read = read_request(&path).unwrap();
    // The swap: same file, a different (stale, so daemon-inadmissible)
    // request that is still validly signed.
    let mut stale_dispatch = dispatch("coder");
    stale_dispatch.run_id = admitted.dispatch.run_id.clone();
    stale_dispatch.jobs[0].description = "something else".into();
    let swapped = JobsDispatchRequest::create(
        stale_dispatch,
        &f.requester,
        0,
        Utc::now() - chrono::Duration::hours(1),
    );
    std::fs::write(&path, serde_yaml_ng::to_string(&swapped).unwrap()).unwrap();

    admit(f.home(), &read, &cfg(f.home()), &bin).unwrap();
    launched(f.home()).expect("launched");
    let kept = mur_track::jobs_dispatch_request::jobs_dispatch_run_dir(
        f.home(),
        &admitted.dispatch.run_id,
    )
    .join(JOBS_DISPATCH_ADMITTED_FILE);
    let child_sees = mur_core::executor::jobs_request::read_request(&kept).unwrap();
    assert_eq!(child_sees.dispatch, admitted.dispatch);
    assert_eq!(child_sees.requested_at, admitted.requested_at);
}
