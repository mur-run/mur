//! Daemon half of #2116. The fake `mur` records its argv and environment so
//! the tests can see exactly what the daemon launched, and with what key path.

use super::*;
use chrono::Utc;
use mur_track::fleet_dispatch_request::FleetDispatch;

struct Fixture {
    tmp: tempfile::TempDir,
    requester: AgentIdentity,
}

impl Fixture {
    fn home(&self) -> &Path {
        self.tmp.path()
    }
}

/// MUR home with agent `mur` (real identity), fleet `council` routed by
/// `orchestrator`, and `mur` allowed to run it.
fn fixture(allow: &str) -> Fixture {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let mur_home = home.join("agents/mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    let requester = AgentIdentity::generate();
    requester.save(&mur_home).unwrap();
    std::fs::write(
        home.join("config.yaml"),
        format!("fleet_run:\n  agents: [mur]\n  fleets: [{allow}]\n"),
    )
    .unwrap();
    for (name, router) in [("council", "orchestrator"), ("own", "mur")] {
        let dir = home.join("fleets").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("fleet.yaml"),
            format!(
                "name: {name}\nrouter: {router}\ngoal: g\nchannel_id: fleet-{name}\nmembers: [a, b]\n"
            ),
        )
        .unwrap();
    }
    Fixture { tmp, requester }
}

/// An absolute cwd on every CI platform: `/work` is not absolute on Windows.
#[cfg(windows)]
const WORK: &str = r"C:\work";
#[cfg(not(windows))]
const WORK: &str = "/work";

fn dispatch(fleet: &str, n: u32) -> FleetDispatch {
    FleetDispatch {
        requested_by: "mur".into(),
        fleet: fleet.into(),
        goal: Some("decide".into()),
        job: None,
        cwd: Some(WORK.into()),
        cwd_inferred: false,
        run_id: format!("fleet-{fleet}-{n}"),
    }
}

fn drop_request(home: &Path, req: &FleetDispatchRequest) -> PathBuf {
    let dir = home.join(FLEET_DISPATCH_REQUEST_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{}.yaml", req.dispatch.run_id));
    std::fs::write(&p, serde_yaml_ng::to_string(req).unwrap()).unwrap();
    p
}

/// `None` when this sandbox refuses to exec under $TMPDIR (unrelated to the
/// behaviour under test), so the caller skips instead of failing.
#[cfg(unix)]
fn fake_mur(home: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let fake = home.join("fake-mur");
    let out = home.join("launched");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n{{ echo \"$@\"; env | grep -c '^{}=' ; }} > {}.tmp && mv {}.tmp {}\n",
            mur_common::identity::SIGNING_HANDOFF_ENV,
            out.display(),
            out.display(),
            out.display()
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
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn record(home: &Path, run_id: &str) -> Option<FleetDispatchRecord> {
    let p = home
        .join(mur_common::paths::RUNS)
        .join(run_id)
        .join(FLEET_DISPATCH_RECORD_FILE);
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn cfg(home: &Path) -> Config {
    Config::try_load(&home.join("config.yaml")).unwrap()
}

#[cfg(unix)]
#[test]
fn verified_request_launches_without_a_handoff_and_records_provenance() {
    let f = fixture("council");
    let Some(fake) = fake_mur(f.home()) else {
        eprintln!("skipping: this sandbox cannot exec under $TMPDIR");
        return;
    };
    let req = FleetDispatchRequest::create(dispatch("council", 1), &f.requester, 0, Utc::now());
    let p = drop_request(f.home(), &req);
    sweep(
        f.home(),
        &f.home().join(FLEET_DISPATCH_REQUEST_DIR),
        &cfg(f.home()),
        &fake,
    )
    .unwrap();
    assert!(!p.exists(), "request must be consumed");

    let out = launched(f.home()).expect("the daemon launched the fleet");
    let mut lines = out.lines();
    assert_eq!(
        lines.next().unwrap(),
        format!("fleet run council --loop --goal=decide --run-id fleet-council-1 --cwd {WORK}")
    );
    // The point of #2116: no stdin handoff, so the child reads the ROUTER's
    // key from disk instead of being handed the requester's.
    assert_eq!(lines.next().unwrap().trim(), "0", "no signing handoff env");

    let rec = record(f.home(), "fleet-council-1").expect("dispatch record");
    assert_eq!(rec.status, FleetDispatchStatus::Launched);
    assert_eq!(rec.dispatched_by, "mur");
    assert_eq!(rec.router.as_deref(), Some("orchestrator"));
    assert!(
        f.home().join("runs/fleet-council-1/fleet_run.log").exists(),
        "the log path the handle promised exists"
    );
}

#[cfg(unix)]
#[test]
fn a_replayed_request_is_rejected_and_keeps_the_original_record() {
    let f = fixture("council");
    let Some(fake) = fake_mur(f.home()) else {
        return;
    };
    let req = FleetDispatchRequest::create(dispatch("council", 1), &f.requester, 0, Utc::now());
    let dir = f.home().join(FLEET_DISPATCH_REQUEST_DIR);
    drop_request(f.home(), &req);
    sweep(f.home(), &dir, &cfg(f.home()), &fake).unwrap();
    launched(f.home()).unwrap();
    std::fs::remove_file(f.home().join("launched")).unwrap();

    drop_request(f.home(), &req);
    sweep(f.home(), &dir, &cfg(f.home()), &fake).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !f.home().join("launched").exists(),
        "replay must not launch"
    );
    assert_eq!(
        record(f.home(), "fleet-council-1").unwrap().status,
        FleetDispatchStatus::Launched
    );
}

/// Builds one tampered or unauthorized request from a fixture.
type MakeRequest = Box<dyn Fn(&Fixture) -> FleetDispatchRequest>;

#[test]
fn rejections_never_launch() {
    let bin = PathBuf::from("/nonexistent/mur");
    let cases: Vec<(&str, MakeRequest)> = vec![
        (
            "wrong key",
            Box::new(|_f| {
                FleetDispatchRequest::create(
                    dispatch("council", 1),
                    &AgentIdentity::generate(),
                    0,
                    Utc::now(),
                )
            }),
        ),
        (
            "stale",
            Box::new(|f| {
                FleetDispatchRequest::create(
                    dispatch("council", 1),
                    &f.requester,
                    0,
                    Utc::now() - chrono::Duration::hours(2),
                )
            }),
        ),
        (
            "traversal name",
            Box::new(|f| {
                let mut r = FleetDispatchRequest::create(
                    dispatch("council", 1),
                    &f.requester,
                    0,
                    Utc::now(),
                );
                r.dispatch.requested_by = "../agents/mur".into();
                r
            }),
        ),
        (
            "run id for another fleet",
            Box::new(|f| {
                let mut d = dispatch("council", 1);
                d.run_id = "fleet-other-1".into();
                FleetDispatchRequest::create(d, &f.requester, 0, Utc::now())
            }),
        ),
    ];
    for (label, make) in cases {
        let f = fixture("council");
        let req = make(&f);
        let p = drop_request(f.home(), &req);
        sweep(
            f.home(),
            &f.home().join(FLEET_DISPATCH_REQUEST_DIR),
            &cfg(f.home()),
            &bin,
        )
        .unwrap();
        assert!(!p.exists(), "{label}: consumed");
        // Unverified requests leave no record: an attacker must not be able
        // to write `dispatched_by` for someone else.
        assert!(
            record(f.home(), &req.dispatch.run_id).is_none(),
            "{label}: no record for an unverified request"
        );
    }
}

/// Verified but unauthorized: rejected, and the reason lands where the
/// requester's handle points.
#[test]
fn verified_but_not_allowed_records_the_reason() {
    let bin = PathBuf::from("/nonexistent/mur");
    for (allow, fleet, needle) in [
        ("something-else", "council", "may not run"),
        ("own", "own", "routed by the requester"),
    ] {
        let f = fixture(allow);
        let req = FleetDispatchRequest::create(dispatch(fleet, 1), &f.requester, 0, Utc::now());
        drop_request(f.home(), &req);
        sweep(
            f.home(),
            &f.home().join(FLEET_DISPATCH_REQUEST_DIR),
            &cfg(f.home()),
            &bin,
        )
        .unwrap();
        let rec = record(f.home(), &req.dispatch.run_id).expect("record");
        assert_eq!(rec.status, FleetDispatchStatus::Rejected, "{fleet}");
        assert_eq!(rec.dispatched_by, "mur");
        assert!(
            rec.reason.as_deref().unwrap_or("").contains(needle),
            "{fleet}: {:?}",
            rec.reason
        );
    }
}

#[test]
fn self_delegating_fleet_is_rejected() {
    let f = fixture("council");
    std::fs::write(
        f.home().join("fleets/council/fleet.yaml"),
        "name: council\nrouter: orchestrator\ngoal: g\nchannel_id: fleet-council\nmembers: [mur, b]\n",
    )
    .unwrap();
    let req = FleetDispatchRequest::create(dispatch("council", 1), &f.requester, 0, Utc::now());
    drop_request(f.home(), &req);
    sweep(
        f.home(),
        &f.home().join(FLEET_DISPATCH_REQUEST_DIR),
        &cfg(f.home()),
        Path::new("/nonexistent/mur"),
    )
    .unwrap();
    let rec = record(f.home(), "fleet-council-1").unwrap();
    assert_eq!(rec.status, FleetDispatchStatus::Rejected);
    assert!(rec.reason.unwrap().contains("self-delegation"));
}

#[cfg(unix)]
#[test]
fn two_requests_for_one_fleet_in_one_sweep_launch_once() {
    let f = fixture("council");
    let Some(fake) = fake_mur(f.home()) else {
        return;
    };
    for n in [1, 2] {
        let req = FleetDispatchRequest::create(dispatch("council", n), &f.requester, 0, Utc::now());
        drop_request(f.home(), &req);
    }
    sweep(
        f.home(),
        &f.home().join(FLEET_DISPATCH_REQUEST_DIR),
        &cfg(f.home()),
        &fake,
    )
    .unwrap();
    let a = record(f.home(), "fleet-council-1").unwrap();
    let b = record(f.home(), "fleet-council-2").unwrap();
    assert_eq!(a.status, FleetDispatchStatus::Launched);
    assert_eq!(b.status, FleetDispatchStatus::Rejected);
    assert!(b.reason.unwrap().contains("earlier in this sweep"));
}

/// Review finding on #2133: submission → rejection → polling the returned
/// handle must report the reason, not "no run recorded". Drives the real
/// agent tool, the real sweep, and the same `status_of` `mur_job_status` reads.
#[tokio::test]
async fn a_rejected_dispatch_polls_as_failed_with_its_reason() {
    use mur_agent_runtime::tools::ToolExecutor;
    // Allowed when submitted, revoked before the sweep: the daemon must
    // re-check from config, and the handle must say why it never started.
    let f = fixture("council");
    write_heartbeat(f.home(), Utc::now()).unwrap();
    let _env = mur_common::test_env::EnvGuard::set([("MUR_BIN", "/nonexistent/mur")]);
    let tool = mur_agent_runtime::tools::fleet_run::FleetRunTool {
        mur_home: f.home().to_path_buf(),
        agent_name: "mur".into(),
        signing: Some(std::sync::Arc::new(
            AgentIdentity::load(&f.home().join("agents/mur")).unwrap(),
        )),
        key_version: 0,
        session_cwd: None,
    };
    let out = tool
        .execute(serde_json::json!({"fleet": "council", "goal": "decide", "cwd": WORK}))
        .await
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(reply["via"], "daemon");
    let run_id = reply["run_id"].as_str().unwrap().to_string();

    std::fs::write(
        f.home().join("config.yaml"),
        "fleet_run:\n  agents: [mur]\n  fleets: [own]\n",
    )
    .unwrap();
    sweep(
        f.home(),
        &f.home().join(FLEET_DISPATCH_REQUEST_DIR),
        &cfg(f.home()),
        Path::new("/nonexistent/mur"),
    )
    .unwrap();

    let status = mur_core::run_status::status_of(f.home(), &run_id)
        .unwrap()
        .expect("a rejected handle must poll as a run, not 'no run recorded'");
    assert_eq!(status.state, mur_core::run_status::State::Failed);
    assert_eq!(status.run.run_id, run_id);
    let reason = status.run.steps[0].error.as_deref().unwrap();
    assert!(reason.contains("may not run fleet 'council'"), "{reason}");
}

#[test]
fn heartbeat_is_a_whole_file_replaced_in_its_granted_directory() {
    let tmp = tempfile::TempDir::new().unwrap();
    let at = Utc::now();
    write_heartbeat(tmp.path(), at).unwrap();
    write_heartbeat(tmp.path(), at).unwrap();
    let path = mur_common::paths::daemon_heartbeat_path(tmp.path());
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["heartbeat_at"], at.to_rfc3339());
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        vec![mur_common::paths::DAEMON_HEARTBEAT_FILE],
        "no tmp left behind"
    );
}
