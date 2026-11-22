//! #2116: a fleet routed by another agent goes to the daemon, never a local
//! spawn; a same-router fleet keeps the local handoff path.

use super::daemon_request::{daemon_alive, write_request};
use super::*;
use mur_track::fleet_dispatch_request::{FLEET_DISPATCH_REQUEST_DIR, FleetDispatchRequest};

/// An absolute cwd on every CI platform: `/work` is not absolute on Windows.
#[cfg(windows)]
const WORK: &str = r"C:\work";
#[cfg(not(windows))]
const WORK: &str = "/work";

/// The agent whose key signs a run's `System` events.
const CONTROL_PLANE: &str = mur_channel::verify::CONTROL_PLANE_AGENT;
/// Any agent that is not the control plane.
const OTHER_AGENT: &str = "orchestrator";

fn home_with(router: Option<&str>) -> tempfile::TempDir {
    home_for(CONTROL_PLANE, router)
}

/// A home whose `fleet_run` allowlist names `agent`, with one fleet, `council`.
fn home_for(agent: &str, router: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::write(
        home.join("config.yaml"),
        format!("fleet_run:\n  agents: [{agent}]\n  fleets: [council]\n"),
    )
    .unwrap();
    let dir = home.join("fleets/council");
    std::fs::create_dir_all(&dir).unwrap();
    let router_line = router.map(|r| format!("router: {r}\n")).unwrap_or_default();
    std::fs::write(
        dir.join("fleet.yaml"),
        format!(
            "name: council\n{router_line}goal: g\nchannel_id: fleet-council\nmembers: [a, b]\n"
        ),
    )
    .unwrap();
    tmp
}

fn beat(home: &std::path::Path, at: chrono::DateTime<chrono::Utc>) {
    let path = mur_common::paths::daemon_heartbeat_path(home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::json!({"pid": 1, "started_at": at, "heartbeat_at": at}).to_string(),
    )
    .unwrap();
}

fn tool(home: &std::path::Path, id: Option<mur_common::identity::AgentIdentity>) -> FleetRunTool {
    tool_as(CONTROL_PLANE, home, id)
}

fn tool_as(
    agent: &str,
    home: &std::path::Path,
    id: Option<mur_common::identity::AgentIdentity>,
) -> FleetRunTool {
    FleetRunTool {
        mur_home: home.to_path_buf(),
        agent_name: agent.into(),
        signing: id.map(std::sync::Arc::new),
        key_version: 0,
        session_cwd: None,
    }
}

fn pending(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(home.join(FLEET_DISPATCH_REQUEST_DIR))
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn cross_router_fleet_writes_a_signed_request_instead_of_spawning() {
    let tmp = home_with(Some("orchestrator"));
    let home = tmp.path();
    beat(home, chrono::Utc::now());
    // Any spawn would fail loudly: this binary does not exist.
    let _env = mur_common::test_env::EnvGuard::set([("MUR_BIN", "/nonexistent/mur")]);
    let id = mur_common::identity::AgentIdentity::generate();
    let pk = id.verifying_key_bytes();
    let out = tool(home, Some(id))
        .execute(serde_json::json!({"fleet": "council", "goal": "decide", "cwd": WORK}))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["status"], "dispatched");
    assert_eq!(v["via"], "daemon");
    assert_eq!(v["signed_as"], "orchestrator");
    let run_id = v["run_id"].as_str().unwrap();

    let files = pending(home);
    assert_eq!(files.len(), 1, "exactly one request: {files:?}");
    let req: FleetDispatchRequest =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&files[0]).unwrap()).unwrap();
    assert!(
        req.verify(&pk),
        "request must verify under the caller's key"
    );
    assert_eq!(req.dispatch.requested_by, "mur");
    assert_eq!(req.dispatch.run_id, run_id, "the handle is the run's id");
    assert_eq!(req.dispatch.goal.as_deref(), Some("decide"));
    assert_eq!(req.dispatch.cwd.as_deref(), Some(WORK));
    assert!(!req.dispatch.cwd_inferred);
}

#[tokio::test]
async fn cross_router_refuses_when_the_daemon_is_down() {
    let tmp = home_with(Some("orchestrator"));
    let home = tmp.path();
    // A heartbeat from an hour ago: the daemon is gone.
    beat(home, chrono::Utc::now() - chrono::Duration::hours(1));
    let err = tool(home, Some(mur_common::identity::AgentIdentity::generate()))
        .execute(serde_json::json!({"fleet": "council"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("mur daemon start"), "{err}");
    assert!(
        pending(home).is_empty(),
        "no request may be queued into nothing"
    );
}

#[tokio::test]
async fn cross_router_refuses_without_a_signing_key() {
    let tmp = home_with(Some("orchestrator"));
    let home = tmp.path();
    beat(home, chrono::Utc::now());
    let err = tool(home, None)
        .execute(serde_json::json!({"fleet": "council"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("signing key"), "{err}");
    assert!(pending(home).is_empty());
}

/// Same-router dispatch is unchanged: no request file, the local spawn runs
/// (and here fails on the missing binary, which proves it was attempted).
#[tokio::test]
async fn same_router_fleet_keeps_the_local_spawn_path() {
    for router in [None, Some("mur"), Some("MUR")] {
        let tmp = home_with(router);
        let home = tmp.path();
        beat(home, chrono::Utc::now());
        let _env = mur_common::test_env::EnvGuard::set([("MUR_BIN", "/nonexistent/mur")]);
        let err = tool(home, Some(mur_common::identity::AgentIdentity::generate()))
            .execute(serde_json::json!({"fleet": "council"}))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("/nonexistent/mur"),
            "router {router:?}: {err}"
        );
        assert!(pending(home).is_empty(), "router {router:?}");
    }
}

/// #2081 items 5 and 6: a locally spawned child holds only the trigger
/// agent's key. When that agent is not the control plane, the run's `System`
/// events (`run_paused`, the stop event, HITL requests) would go out unsigned
/// even though the agent routes the fleet itself. So this run goes to the
/// daemon as well: one signed request, and no local spawn.
#[tokio::test]
async fn self_routed_fleet_of_another_agent_goes_to_the_daemon() {
    let tmp = home_for(OTHER_AGENT, Some(OTHER_AGENT));
    let home = tmp.path();
    beat(home, chrono::Utc::now());
    // Any spawn would fail loudly: this binary does not exist.
    let _env = mur_common::test_env::EnvGuard::set([("MUR_BIN", "/nonexistent/mur")]);
    let id = mur_common::identity::AgentIdentity::generate();
    let pk = id.verifying_key_bytes();
    let out = tool_as(OTHER_AGENT, home, Some(id))
        .execute(serde_json::json!({"fleet": "council", "goal": "decide", "cwd": WORK}))
        .await
        .expect("a self-routed run of a non-control-plane agent must not spawn locally");
    let v: serde_json::Value = serde_json::from_str(&out.text).unwrap();
    assert_eq!(v["status"], "dispatched");
    assert_eq!(v["via"], "daemon");
    assert_eq!(v["signed_as"], OTHER_AGENT);

    let files = pending(home);
    assert_eq!(files.len(), 1, "exactly one request: {files:?}");
    let req: FleetDispatchRequest =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&files[0]).unwrap()).unwrap();
    assert!(
        req.verify(&pk),
        "request must verify under the caller's key"
    );
    assert_eq!(req.dispatch.requested_by, OTHER_AGENT);
    assert_eq!(req.dispatch.run_id, v["run_id"].as_str().unwrap());
}

#[test]
fn daemon_alive_reads_the_heartbeat_not_the_pid() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let now = chrono::Utc::now();
    assert!(!daemon_alive(home, 60, now), "no heartbeat file");
    beat(home, now - chrono::Duration::seconds(30));
    assert!(daemon_alive(home, 60, now));
    assert!(!daemon_alive(home, 10, now));
    std::fs::write(mur_common::paths::daemon_heartbeat_path(home), "not json").unwrap();
    assert!(!daemon_alive(home, 60, now));
}

#[test]
fn write_request_leaves_no_tmp_file() {
    let tmp = tempfile::tempdir().unwrap();
    let req = FleetDispatchRequest::create(
        mur_track::fleet_dispatch_request::FleetDispatch {
            requested_by: "mur".into(),
            fleet: "council".into(),
            goal: None,
            job: None,
            cwd: None,
            cwd_inferred: false,
            run_id: "fleet-council-x".into(),
        },
        &mur_common::identity::AgentIdentity::generate(),
        0,
        chrono::Utc::now(),
    );
    write_request(tmp.path(), &req).unwrap();
    let names: Vec<_> = std::fs::read_dir(tmp.path().join(FLEET_DISPATCH_REQUEST_DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["fleet-council-x.yaml".to_string()]);
}

#[test]
fn dispatch_argv_matches_the_local_spawn_shape() {
    let a = dispatch_argv(
        "council",
        Some("g"),
        None,
        "fleet-council-1",
        Some(std::path::Path::new("/w")),
        true,
    );
    assert_eq!(
        a,
        [
            "fleet",
            "run",
            "council",
            "--loop",
            "--goal=g",
            "--run-id",
            "fleet-council-1",
            "--cwd",
            "/w",
            "--cwd-inferred"
        ]
    );
}
