/// The old hint told every spawn failure to "restart the agent so the
/// carve-ins apply". When the binary sat in `$TMPDIR` (outside any exec
/// lane) a restart changed nothing, and an open item was filed on that
/// wrong lead. A denial must name the path/lane cause; a restart is only
/// offered conditionally.
#[test]
fn permission_denied_spawn_names_the_exec_lane_not_a_bare_restart() {
    let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    let msg = super::spawn_error_message(std::path::Path::new("/tmp/x/fake-mur"), &e);
    assert!(msg.contains("/tmp/x/fake-mur"), "{msg}");
    assert!(msg.contains("exec lane"), "{msg}");
    assert!(
        !msg.contains("restart the agent so the fleet_run carve-ins apply"),
        "{msg}"
    );
    assert!(
        msg.contains("fleet_run.agents"),
        "restart must be conditional: {msg}"
    );
}

#[test]
fn non_permission_spawn_errors_do_not_blame_the_sandbox() {
    let e = std::io::Error::from(std::io::ErrorKind::NotFound);
    let msg = super::spawn_error_message(std::path::Path::new("mur"), &e);
    assert!(!msg.contains("sandbox"), "{msg}");
    assert!(!msg.contains("restart"), "{msg}");
}

use super::*;

fn write_config(home: &std::path::Path, yaml: &str) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(home.join("config.yaml"), yaml).unwrap();
}

fn write_fleet(home: &std::path::Path, name: &str, budget: f64) {
    let dir = home.join("fleets").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("fleet.yaml"),
        format!(
            "name: {name}\ngoal: g\nchannel_id: fleet-{name}\nmembers: []\nloop:\n  trigger: manual\n  budget_usd: {budget}\n"
        ),
    )
    .unwrap();
}

#[test]
fn allowed_is_deny_by_default() {
    let cfg = mur_common::config::FleetRunConfig::default();
    assert!(!allowed(&cfg, "mur", "deep-research"));
    let cfg = mur_common::config::FleetRunConfig {
        agents: vec!["mur".into()],
        fleets: vec!["deep-research".into()],
    };
    assert!(allowed(&cfg, "mur", "deep-research"));
    assert!(!allowed(&cfg, "dr_worker_1", "deep-research"));
    assert!(!allowed(&cfg, "mur", "other"));
}

#[tokio::test]
async fn denies_unauthorized_agent() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(tmp.path(), "{}");
    let tool = FleetRunTool {
        mur_home: tmp.path().to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    };
    let err = tool
        .execute(serde_json::json!({"fleet": "deep-research"}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::NotAuthorized(_)),
        "a refusal is NotAuthorized, not Execution: {err}"
    );
    assert!(err.to_string().contains("not authorized"), "{err}");
}

#[tokio::test]
async fn refuses_a_fleet_that_lists_the_triggering_agent() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(
        tmp.path(),
        "fleet_run:\n  agents: [mur]\n  fleets: [selfy]\n",
    );
    let dir = tmp.path().join("fleets").join("selfy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("fleet.yaml"),
        "name: selfy\ngoal: g\nchannel_id: fleet-selfy\nmembers: [qa, Mur]\nloop:\n  trigger: manual\n  budget_usd: 1.0\n",
    )
    .unwrap();
    let tool = FleetRunTool {
        mur_home: tmp.path().to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    };
    let err = tool
        .execute(serde_json::json!({"fleet": "selfy"}))
        .await
        .unwrap_err();
    // Matched case-insensitively, like every other agent-name lookup.
    assert!(err.to_string().contains("as a member"), "{err}");
}

#[tokio::test]
async fn rejects_invalid_fleet_name() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(tmp.path(), "{}");
    let tool = FleetRunTool {
        mur_home: tmp.path().to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    };
    let err = tool
        .execute(serde_json::json!({"fleet": "../etc"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("invalid fleet name"), "{err}");
}

#[test]
fn agent_enabled_gates_registration() {
    let tmp = tempfile::tempdir().unwrap();
    let load = |y: &str| {
        write_config(tmp.path(), y);
        mur_common::config::Config::load_or_default(&tmp.path().join("config.yaml")).fleet_run
    };
    assert!(!enabled_in(&load("{}"), "mur"));
    let cfg = load("fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n");
    assert!(enabled_in(&cfg, "mur"));
    assert!(!enabled_in(&cfg, "dr_worker_1"));
}

#[test]
fn def_schema_requires_fleet() {
    let tool = FleetRunTool {
        mur_home: PathBuf::from("/tmp"),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    };
    let def = tool.def();
    assert_eq!(def.name, FLEET_RUN);
    assert_eq!(def.input_schema["required"][0], "fleet");
}

#[test]
fn live_progress_record_exposes_its_run_id() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = mur_common::paths::fleet_state_dir(tmp.path(), "deep-research");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(mur_common::paths::FLEET_PROGRESS_FILE),
        r#"{"run_id":"run-live","finished_at":null}"#,
    )
    .unwrap();
    assert_eq!(
        live_run_id(tmp.path(), "deep-research").as_deref(),
        Some("run-live")
    );
}

#[tokio::test]
async fn refuses_when_the_fleet_progress_slot_is_live() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(
        tmp.path(),
        "fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n",
    );
    write_fleet(tmp.path(), "deep-research", 0.0);
    let dir = mur_common::paths::fleet_state_dir(tmp.path(), "deep-research");
    std::fs::create_dir_all(&dir).unwrap();
    let progress = dir.join(mur_common::paths::FLEET_PROGRESS_FILE);
    std::fs::write(progress, r#"{"run_id":"run-live","finished_at":null}"#).unwrap();
    let tool = FleetRunTool {
        mur_home: tmp.path().to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    };
    let err = tool
        .execute(serde_json::json!({"fleet": "deep-research", "goal": "q"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already live"), "{err}");
    assert!(err.to_string().contains("run-live"), "{err}");
}

/// §3.6: the tool returns a handle within a second while the fleet keeps
/// running, and the child got the id it will be polled under. nextest
/// runs each test in its own process, so MUR_BIN is private to this one.
#[cfg(unix)]
#[tokio::test]
async fn fleet_run_returns_a_handle_and_leaves_the_child_running() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_config(
        home,
        "fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n",
    );
    write_fleet(home, "deep-research", 0.0);
    let argv_log = home.join("argv.txt");
    let fake = home.join("fake-mur");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"$@\" > {}\nsleep 30\n",
            argv_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The fake mur lives under $TMPDIR. An exec sandbox (e.g. the MUR
    // agent runtime) refuses to exec anything there, which would fail
    // this test for a reason unrelated to fleet_run. Probe with a no-op
    // script in the same directory and skip only when exec itself is denied.
    let probe = home.join("exec-probe");
    std::fs::write(&probe, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !std::process::Command::new(&probe)
        .status()
        .is_ok_and(|s| s.success())
    {
        eprintln!("skipping: this sandbox cannot exec under $TMPDIR");
        return;
    }
    let _env = mur_common::test_env::EnvGuard::set([("MUR_BIN", &fake)]);
    let tool = FleetRunTool {
        mur_home: home.to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    };
    let t0 = std::time::Instant::now();
    let out = tool
        .execute(serde_json::json!({"fleet": "deep-research", "goal": "why is the sky blue"}))
        .await
        .unwrap();
    // The fake mur binary sleeps 30s, so a blocking dispatch cannot come
    // back under that. 10s leaves headroom for a loaded machine running
    // the whole workspace; 4s flaked there, 2s was measuring the runner.
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(10),
        "returned in {:?}",
        t0.elapsed()
    );
    let v: serde_json::Value = serde_json::from_str(&out.text).expect("json handle");
    assert_eq!(v["status"], "dispatched");
    let run_id = v["run_id"].as_str().unwrap().to_string();
    assert!(run_id.starts_with("fleet-deep-research-"), "{run_id}");
    assert!(v["follow"].as_str().unwrap().contains("mur_job_status"));
    // the child is still alive and was told the id. Poll instead of a
    // fixed sleep: under load `sh` may not have reached `echo` yet
    // (NotFound), or `>` has truncated the file but not written it. The
    // line is complete once it ends in the newline `echo` appends.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let argv = loop {
        match std::fs::read_to_string(&argv_log) {
            Ok(s) if s.ends_with('\n') => break s,
            other => assert!(
                std::time::Instant::now() < deadline,
                "child never wrote its argv within 10s: {other:?}"
            ),
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    assert!(argv.contains(&format!("--run-id {run_id}")), "{argv}");
    assert!(
        argv.starts_with("deep-research why is the sky blue"),
        "{argv}"
    );
    assert!(std::path::Path::new(v["log"].as_str().unwrap()).exists());
    // The dispatch deliberately detaches the child; don't leave the fake
    // `sleep 30` orphaned past the test. Matched on the per-test path.
    let _ = std::process::Command::new("pkill")
        .args(["-f", &fake.display().to_string()])
        .status();
}

/// A `mur` stand-in that records its argv and lingers, plus the file it
/// writes to. `None` when this sandbox refuses to exec under $TMPDIR —
/// the caller skips, since that failure is not about fleet_run.
#[cfg(unix)]
fn fake_mur(home: &std::path::Path) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    use std::os::unix::fs::PermissionsExt;
    let argv_log = home.join("argv.txt");
    let fake = home.join("fake-mur");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"$@\" > {}\nsleep 30\n",
            argv_log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let probe = home.join("exec-probe");
    std::fs::write(&probe, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !std::process::Command::new(&probe)
        .status()
        .is_ok_and(|s| s.success())
    {
        eprintln!("skipping: this sandbox cannot exec under $TMPDIR");
        return None;
    }
    Some((fake, argv_log))
}

#[cfg(unix)]
async fn argv_of(argv_log: &std::path::Path) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok(s) = std::fs::read_to_string(argv_log)
            && s.ends_with('\n')
        {
            return s;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child never wrote its argv within 10s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// #1607: the child is told the SESSION cwd on argv. Where this process
/// sits is irrelevant — the runtime's cwd is its home, not the project.
/// No explicit `cwd` → the session dir, marked inferred; an explicit one
/// → verbatim, not marked.
#[cfg(unix)]
#[tokio::test]
async fn fleet_run_passes_session_cwd_on_argv_not_process_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_config(home, "fleet_run:\n  agents: [mur]\n  fleets: [coder]\n");
    write_fleet(home, "coder", 0.0);
    let Some((fake, argv_log)) = fake_mur(home) else {
        return;
    };
    let _env = mur_common::test_env::EnvGuard::set([("MUR_BIN", &fake)]);
    let project = home.join("the-project");
    std::fs::create_dir_all(&project).unwrap();
    let session_cwd = crate::tools::fs_policy::SessionCwd::new(home.to_path_buf());
    let _ = session_cwd.set(project.clone());
    let tool = FleetRunTool {
        mur_home: home.to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: Some(session_cwd),
    };
    let process_cwd = std::env::current_dir().unwrap();
    assert_ne!(process_cwd, project);

    tool.execute(serde_json::json!({"fleet": "coder", "goal": "build it"}))
        .await
        .unwrap();
    let argv = argv_of(&argv_log).await;
    assert!(
        argv.starts_with("fleet run coder --loop --goal=build it"),
        "{argv}"
    );
    assert!(
        argv.contains(&format!("--cwd {} --cwd-inferred", project.display())),
        "{argv}"
    );
    assert!(
        !argv.contains(&process_cwd.display().to_string()),
        "process cwd leaked into argv: {argv}"
    );
    let _ = std::process::Command::new("pkill")
        .args(["-f", &fake.display().to_string()])
        .status();

    // Second dispatch with an explicit target: a fresh fleet, since one
    // live run per fleet is the rule.
    write_config(
        home,
        "fleet_run:\n  agents: [mur]\n  fleets: [coder, other]\n",
    );
    write_fleet(home, "other", 0.0);
    let _ = std::fs::remove_file(&argv_log);
    let explicit = home.join("elsewhere");
    std::fs::create_dir_all(&explicit).unwrap();
    tool.execute(serde_json::json!({
        "fleet": "other", "goal": "g", "cwd": explicit.display().to_string()
    }))
    .await
    .unwrap();
    let argv = argv_of(&argv_log).await;
    assert!(
        argv.contains(&format!("--cwd {}", explicit.display())),
        "{argv}"
    );
    assert!(!argv.contains("--cwd-inferred"), "{argv}");
    let _ = std::process::Command::new("pkill")
        .args(["-f", &fake.display().to_string()])
        .status();
}

#[tokio::test]
async fn fleet_run_rejects_relative_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(
        tmp.path(),
        "fleet_run:\n  agents: [mur]\n  fleets: [coder]\n",
    );
    write_fleet(tmp.path(), "coder", 0.0);
    let tool = FleetRunTool {
        mur_home: tmp.path().to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    };
    let err = tool
        .execute(serde_json::json!({"fleet": "coder", "goal": "g", "cwd": "rel/dir"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("absolute"), "{err}");
}

/// `goal` overrides the run's goal (loop + `--goal=`), `job` is one-shot,
/// both together queue the job and loop toward the goal; nothing → plain
/// loop on the standing goal. deep-research keeps its own verb.
#[test]
fn child_args_maps_goal_and_job() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        child_args("coder", Some("ship it"), None),
        v(&["fleet", "run", "coder", "--loop", "--goal=ship it"])
    );
    assert_eq!(
        child_args("coder", None, Some("fix #1")),
        v(&["fleet", "run", "coder", "fix #1"])
    );
    assert_eq!(
        child_args("coder", Some("ship it"), Some("fix #1")),
        v(&[
            "fleet",
            "run",
            "coder",
            "fix #1",
            "--loop",
            "--goal=ship it"
        ])
    );
    assert_eq!(
        child_args("coder", None, None),
        v(&["fleet", "run", "coder", "--loop"])
    );
    // A goal that looks like a flag stays one value.
    assert_eq!(
        child_args("coder", Some("--yes"), None),
        v(&["fleet", "run", "coder", "--loop", "--goal=--yes"])
    );
    assert_eq!(
        child_args(DEEP_RESEARCH, Some("why"), None),
        v(&[DEEP_RESEARCH, "why"])
    );
}

#[tokio::test]
async fn deep_research_refuses_a_job() {
    let tmp = tempfile::tempdir().unwrap();
    write_config(
        tmp.path(),
        "fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n",
    );
    let tool = FleetRunTool {
        mur_home: tmp.path().to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    };
    let err = tool
        .execute(serde_json::json!({"fleet": "deep-research", "job": "x"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("`goal`"), "{err}");
}
