//! #2379: the built-in `parallel_jobs` asks the daemon; it never runs jobs.

use super::*;
use mur_common::agent::McpServerEntry;

#[cfg(windows)]
const WORK: &str = r"C:\work";
#[cfg(not(windows))]
const WORK: &str = "/work";

fn home(config: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.yaml"), config).unwrap();
    tmp
}

const CFG: &str = "parallel_jobs:\n  targets: [coder, qa]\n";

fn beat(home: &Path, at: chrono::DateTime<chrono::Utc>) {
    let path = mur_common::paths::daemon_heartbeat_path(home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::json!({"pid": 1, "started_at": at, "heartbeat_at": at}).to_string(),
    )
    .unwrap();
}

fn tool(home: &Path, id: Option<AgentIdentity>) -> ParallelJobsTool {
    ParallelJobsTool {
        mur_home: home.to_path_buf(),
        agent_name: "pm".into(),
        signing: id.map(Arc::new),
        key_version: 0,
        session_cwd: None,
    }
}

fn pending(home: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(home.join(JOBS_DISPATCH_REQUEST_DIR))
        .map(|d| {
            d.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
                .collect()
        })
        .unwrap_or_default()
}

fn input() -> serde_json::Value {
    serde_json::json!({
        "jobs": [{"description": "build it", "agent": "coder"}, {"description": "test it"}],
        "agent": "qa",
        "cwd": WORK,
    })
}

#[test]
fn parse_applies_the_default_agent_and_clamps_concurrency() {
    let mut v = input();
    v["max_concurrency"] = serde_json::json!(999);
    let p = parse_input(&v, None).unwrap();
    assert_eq!(p.jobs[0].agent, "coder");
    assert_eq!(p.jobs[1].agent, "qa");
    assert_eq!(p.max_concurrency as usize, MAX_JOBS);
    assert_eq!(p.cwd.as_deref(), Some(Path::new(WORK)));
    assert!(!p.cwd_inferred);
}

#[test]
fn parse_refuses_bad_shapes() {
    for bad in [
        serde_json::json!({}),
        serde_json::json!({"jobs": []}),
        serde_json::json!({"jobs": [{"description": "x"}]}),
        serde_json::json!({"jobs": [{"description": "x", "agent": "qa"}], "cwd": "rel/dir"}),
    ] {
        assert!(
            matches!(parse_input(&bad, None), Err(ToolError::InvalidInput(_))),
            "{bad}"
        );
    }
}

#[test]
fn precheck_names_the_config_key_that_refused() {
    let cfg: ParallelJobsConfig = serde_yaml_ng::from_str("targets: [coder]").unwrap();
    let job = |a: &str| {
        vec![RequestedJob {
            description: "d".into(),
            agent: a.into(),
        }]
    };
    assert!(precheck(&cfg, "pm", &job("Coder")).is_ok(), "case-folded");
    let e = precheck(&cfg, "pm", &job("qa")).unwrap_err().to_string();
    assert!(e.contains("parallel_jobs.targets"), "{e}");
    assert!(matches!(
        precheck(&cfg, "pm", &job("pm")),
        Err(ToolError::InvalidInput(_))
    ));

    let gated: ParallelJobsConfig =
        serde_yaml_ng::from_str("targets: [coder]\nrequesters: [mur]").unwrap();
    let e = precheck(&gated, "pm", &job("coder"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("parallel_jobs.requesters"), "{e}");
}

#[test]
fn enabled_needs_targets_and_respects_requesters() {
    let none: ParallelJobsConfig = serde_yaml_ng::from_str("targets: []").unwrap();
    assert!(!enabled_in(&none, "pm"), "no targets: nothing to offer");
    let any: ParallelJobsConfig = serde_yaml_ng::from_str("targets: [coder]").unwrap();
    assert!(enabled_in(&any, "pm"), "empty requesters = any agent (Q1)");
    let gated: ParallelJobsConfig =
        serde_yaml_ng::from_str("targets: [coder]\nrequesters: [mur]").unwrap();
    assert!(enabled_in(&gated, "mur"));
    assert!(!enabled_in(&gated, "pm"));
}

#[test]
fn only_murs_own_mcp_server_is_shadowed() {
    let entry = |name: &str, cmd: &str| McpServerEntry {
        name: name.into(),
        command: cmd.into(),
        ..serde_yaml_ng::from_str("name: x\ncommand: x").unwrap()
    };
    let names = shadowed_mcp_names(&[
        entry("mur-tools", "/Users/u/.mur/mcp-servers/mur-mcp-server"),
        entry("bare", "mur-mcp-server"),
        entry("other", "/opt/bin/jobs-server"),
    ]);
    assert_eq!(
        names,
        vec![
            "mcp__mur-tools__parallel_jobs".to_string(),
            "mcp__bare__parallel_jobs".to_string()
        ]
    );
}

#[tokio::test]
async fn a_dead_daemon_refuses_and_writes_nothing() {
    let h = home(CFG);
    let err = tool(h.path(), Some(AgentIdentity::generate()))
        .execute(input())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("mur daemon start"), "{err}");
    assert!(pending(h.path()).is_empty());
}

#[tokio::test]
async fn no_key_refuses_and_writes_nothing() {
    let h = home(CFG);
    beat(h.path(), chrono::Utc::now());
    assert!(tool(h.path(), None).execute(input()).await.is_err());
    assert!(pending(h.path()).is_empty());
}

#[tokio::test]
async fn a_target_outside_the_allowlist_writes_nothing() {
    let h = home("parallel_jobs:\n  targets: [coder]\n");
    beat(h.path(), chrono::Utc::now());
    let err = tool(h.path(), Some(AgentIdentity::generate()))
        .execute(input())
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotAuthorized(_)), "{err}");
    assert!(pending(h.path()).is_empty());
}

#[tokio::test]
async fn a_live_daemon_gets_a_signed_request_matching_the_handle() {
    let h = home(CFG);
    beat(h.path(), chrono::Utc::now());
    let id = AgentIdentity::generate();
    let pubkey = id.verifying_key_bytes();
    let mut v = input();
    v["yes"] = serde_json::json!(true);
    let out = tool(h.path(), Some(id)).execute(v).await.unwrap();
    let reply: serde_json::Value = serde_json::from_str(&out.text).unwrap();

    let files = pending(h.path());
    assert_eq!(files.len(), 1);
    let req: JobsDispatchRequest =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&files[0]).unwrap()).unwrap();
    assert!(req.verify(&pubkey), "signed with the requester's own key");
    assert_eq!(req.dispatch.requested_by, "pm");
    assert_eq!(reply["run_id"], req.dispatch.run_id.as_str());
    assert_eq!(reply["channel_id"], req.dispatch.channel_id.as_str());
    assert_eq!(reply["status"], "dispatched");
    assert!(req.dispatch.check_shape().is_ok());
    // Q3: the request has no way to carry it, and the model is told.
    assert!(!std::fs::read_to_string(&files[0]).unwrap().contains("yes"));
    assert!(
        reply["note"]
            .as_str()
            .unwrap()
            .contains("`yes` was ignored")
    );
}

fn map_with_mcp() -> std::collections::HashMap<String, Arc<dyn ToolExecutor>> {
    let mut m: std::collections::HashMap<String, Arc<dyn ToolExecutor>> = Default::default();
    for n in [
        "mcp__mur-tools__parallel_jobs",
        "mcp__mur-tools__mur_job_status",
    ] {
        // Any executor will do: only the map key is under test.
        let stand_in = tool(Path::new("/nonexistent"), None);
        m.insert(n.into(), Arc::new(stand_in));
    }
    m
}

fn mur_tools() -> Vec<McpServerEntry> {
    vec![McpServerEntry {
        name: "mur-tools".into(),
        command: "mur-mcp-server".into(),
        ..serde_yaml_ng::from_str("name: x\ncommand: x").unwrap()
    }]
}

#[test]
fn register_replaces_the_mcp_tool_and_keeps_its_siblings() {
    let h = home(CFG);
    let cfg: ParallelJobsConfig = serde_yaml_ng::from_str("targets: [coder]").unwrap();
    let mut map = map_with_mcp();
    let gone = register(&mut map, &cfg, &[], &mur_tools(), tool(h.path(), None));
    assert_eq!(gone, vec!["mcp__mur-tools__parallel_jobs".to_string()]);
    assert!(map.contains_key(PARALLEL_JOBS));
    assert!(!map.contains_key("mcp__mur-tools__parallel_jobs"));
    assert!(
        map.contains_key("mcp__mur-tools__mur_job_status"),
        "status polling stays on MCP"
    );
}

#[test]
fn register_is_a_no_op_when_not_offered_or_denied() {
    let h = home(CFG);
    let off: ParallelJobsConfig = serde_yaml_ng::from_str("targets: []").unwrap();
    let mut map = map_with_mcp();
    assert!(register(&mut map, &off, &[], &mur_tools(), tool(h.path(), None)).is_empty());
    assert!(map.contains_key("mcp__mur-tools__parallel_jobs") && !map.contains_key(PARALLEL_JOBS));

    let on: ParallelJobsConfig = serde_yaml_ng::from_str("targets: [coder]").unwrap();
    let deny: Vec<mur_common::agent::ToolRule> =
        serde_yaml_ng::from_str("- pattern: parallel_jobs\n  policy: deny").unwrap();
    let mut map = map_with_mcp();
    assert!(register(&mut map, &on, &deny, &mur_tools(), tool(h.path(), None)).is_empty());
    assert!(map.contains_key("mcp__mur-tools__parallel_jobs") && !map.contains_key(PARALLEL_JOBS));
}

/// Review P2 (#2386): a deny written for the MCP tool, exact or server-wide,
/// keeps the built-in out too — and leaves the (denied) MCP entry where the
/// policy layer already refuses it.
#[test]
fn a_deny_on_the_replaced_mcp_tool_keeps_the_built_in_out() {
    let h = home(CFG);
    let on: ParallelJobsConfig = serde_yaml_ng::from_str("targets: [coder]").unwrap();
    for pattern in [
        "mcp__mur-tools__parallel_jobs",
        "mcp__mur-tools__*",
        "mcp__*",
    ] {
        let deny: Vec<mur_common::agent::ToolRule> =
            serde_yaml_ng::from_str(&format!("- pattern: \"{pattern}\"\n  policy: deny")).unwrap();
        let mut map = map_with_mcp();
        assert!(
            register(&mut map, &on, &deny, &mur_tools(), tool(h.path(), None)).is_empty(),
            "{pattern}"
        );
        assert!(!map.contains_key(PARALLEL_JOBS), "{pattern}");
        assert!(
            map.contains_key("mcp__mur-tools__parallel_jobs"),
            "{pattern}"
        );
    }
    // An allow on the MCP name (or a deny on an unrelated one) changes nothing.
    let rules: Vec<mur_common::agent::ToolRule> = serde_yaml_ng::from_str(
        "- pattern: mcp__mur-tools__parallel_jobs\n  policy: allow\n- pattern: mcp__other__*\n  policy: deny",
    )
    .unwrap();
    let mut map = map_with_mcp();
    assert!(!register(&mut map, &on, &rules, &mur_tools(), tool(h.path(), None)).is_empty());
    assert!(map.contains_key(PARALLEL_JOBS));
}
