//! #1984: the wiring decision and its profile edits.

use super::*;
use mur_common::test_env::EnvGuard;

fn server(name: &str, command: &str) -> McpServerEntry {
    McpServerEntry {
        name: name.into(),
        command: command.into(),
        ..Default::default()
    }
}

fn profile(servers: &[(&str, &str)]) -> AgentProfile {
    let mut p = AgentProfile::default_for_tests();
    p.name = "carol".into();
    p.mcp_servers = servers.iter().map(|(n, c)| server(n, c)).collect();
    p
}

#[test]
fn no_mur_server_means_attach() {
    assert_eq!(
        slot(&profile(&[("serena", "/x/serena")])).unwrap(),
        Slot::Attach
    );
}

/// The media capability's bare `mur-mcp-server` already serves the tool:
/// a second copy would just double every MUR tool in the agent's list.
#[test]
fn an_existing_mur_server_is_reused_whatever_its_name() {
    for cmd in ["mur-mcp-server", "/u/.mur/mcp-servers/mur-mcp-server"] {
        assert_eq!(
            slot(&profile(&[("media", cmd)])).unwrap(),
            Slot::Existing {
                name: "media".into(),
                enabled: true
            },
            "{cmd}"
        );
    }
}

#[test]
fn a_disabled_mur_server_is_reported_as_disabled() {
    let mut p = profile(&[("media", "mur-mcp-server")]);
    p.disabled_mcp.push("media".into());
    assert_eq!(
        slot(&p).unwrap(),
        Slot::Existing {
            name: "media".into(),
            enabled: false
        }
    );
}

#[test]
fn a_user_server_holding_the_entry_name_is_refused() {
    let err = slot(&profile(&[(ENTRY_NAME, "/opt/other-server")]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("not MUR's server"), "{err}");
    assert!(err.contains("--no-ast-grep"), "{err}");
}

/// A name ending in the server's name is not the server.
#[test]
fn only_the_exact_file_name_counts_as_mur_server() {
    assert!(is_mur_mcp_server("/a/b/mur-mcp-server"));
    assert!(!is_mur_mcp_server("/a/b/not-mur-mcp-server"));
    assert!(!is_mur_mcp_server("/a/mur-mcp-server/x"));
}

#[test]
fn permissions_name_the_bundled_server_and_pinned_binary() {
    let home = Path::new("/mh");
    let server = mur_common::exec::bundled_mcp_server_path_in(home);
    let attach = permissions(home, &Slot::Attach);
    assert!(
        attach.contains(&Permission::Spawn(
            mur_common::config::ast_grep_binary_path(home)
                .display()
                .to_string()
        ))
    );
    assert!(attach.contains(&Permission::Spawn(server.display().to_string())));
    assert!(attach.contains(&Permission::ReadWrite(home.join("runtime/ast-grep"))));
    assert!(attach.contains(&Permission::Mcp {
        name: ENTRY_NAME.into(),
        command: server.clone(),
    }));

    let reuse = permissions(
        home,
        &Slot::Existing {
            name: "media".into(),
            enabled: true,
        },
    );
    assert!(
        !reuse.iter().any(|p| matches!(p, Permission::Mcp { .. })),
        "an existing MUR server gets grants only: {reuse:?}"
    );
}

/// An isolated `MUR_HOME` holding agent `carol` with `servers`.
fn home_with(servers: &[(&str, &str)]) -> (tempfile::TempDir, EnvGuard, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join("mur-home");
    let agent_home = mur_home.join("agents").join("carol");
    std::fs::create_dir_all(&agent_home).unwrap();
    let mut env = EnvGuard::hold();
    env.set_var("MUR_HOME", &mur_home);
    std::fs::write(
        agent_home.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile(servers)).unwrap(),
    )
    .unwrap();
    (tmp, env, mur_home)
}

fn load(mur_home: &Path) -> AgentProfile {
    serde_yaml_ng::from_str(
        &std::fs::read_to_string(mur_home.join("agents/carol/profile.yaml")).unwrap(),
    )
    .unwrap()
}

#[test]
fn apply_wires_once_and_a_rerun_adds_nothing() {
    let (_t, _e, mur_home) = home_with(&[]);
    let mut m = Manifest::default();
    let mut out = Vec::new();
    apply(&mur_home, "carol", &mut m, &mut out).unwrap();
    let first = load(&mur_home);
    apply(&mur_home, "carol", &mut m, &mut out).unwrap();
    let second = load(&mur_home);

    let server = mur_common::exec::bundled_mcp_server_path_in(&mur_home);
    let entries: Vec<_> = second
        .mcp_servers
        .iter()
        .filter(|e| e.name == ENTRY_NAME)
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].command, server.display().to_string());
    assert_eq!(first.mcp_servers, second.mcp_servers);
    assert_eq!(first.entitlements, second.entitlements);

    let run_dir = mur_common::config::ast_grep_runtime_dir(&mur_home);
    assert!(
        run_dir.is_dir(),
        "created, or the seal would drop the grant"
    );
    let (bin, dir) = required_paths(&mur_home);
    assert!(
        crate::cmd::doctor::ast_grep_gaps_for_tests(&second, &bin, &dir).is_empty(),
        "doctor must call a wired agent wired"
    );
    assert_eq!(m.granted.len(), permissions(&mur_home, &Slot::Attach).len());

    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("added MCP server `mur-tools`"), "{out}");
    assert!(out.contains("next start"), "{out}");
}

#[test]
fn apply_reuses_an_existing_server_and_keeps_its_entry() {
    let (_t, _e, mur_home) = home_with(&[("media", "mur-mcp-server")]);
    let before = load(&mur_home).mcp_servers;
    let mut out = Vec::new();
    apply(&mur_home, "carol", &mut Manifest::default(), &mut out).unwrap();
    assert_eq!(load(&mur_home).mcp_servers, before);
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("served by existing MCP server `media`"),
        "{out}"
    );
}

#[test]
fn doctor_lists_every_missing_piece() {
    let p = profile(&[]);
    let gaps = crate::cmd::doctor::ast_grep_gaps_for_tests(
        &p,
        Path::new("/mh/tools/ast-grep/0/ast-grep"),
        Path::new("/mh/runtime/ast-grep"),
    );
    assert_eq!(gaps.len(), 3, "{gaps:?}");
    assert!(gaps[0].contains("no MUR MCP server"), "{gaps:?}");
    assert!(gaps[1].contains("spawn allowlist"), "{gaps:?}");
    assert!(gaps[2].contains("read+write"), "{gaps:?}");
}
