use super::*;
use mur_common::agent::{McpServerEntry, McpServerStatus};
use std::os::unix::fs::PermissionsExt;

fn bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.canonicalize().unwrap()
}

fn profile(cmd: &str) -> Vec<McpServerEntry> {
    vec![McpServerEntry {
        name: "dc".into(),
        command: cmd.into(),
        ..Default::default()
    }]
}

#[test]
fn bare_interpreter_resolving_outside_the_grant_is_predicted() {
    let shim = tempfile::tempdir().unwrap();
    let node = bin(shim.path(), "node", "#!/bin/sh\n");
    let path = std::env::join_paths([shim.path()]).unwrap();
    let got = predict_with(&profile("node"), &no_bundle(), &path, |_| false);
    assert_eq!(
        got,
        vec![Blocked {
            server: "dc".into(),
            wanted: "node".into(),
            resolved: node,
        }]
    );
    assert!(predict_with(&profile("node"), &no_bundle(), &path, |_| true).is_empty());
}

#[test]
fn env_shebang_interpreter_is_followed_one_hop() {
    let d = tempfile::tempdir().unwrap();
    let npx = bin(d.path(), "npx", "#!/usr/bin/env node\n");
    let node = bin(d.path(), "node", "#!/bin/sh\n");
    let path = std::env::join_paths([d.path()]).unwrap();
    let env = Path::new("/usr/bin/env");
    let got = predict_with(&profile("npx"), &no_bundle(), &path, |p| {
        p == npx || p == env
    });
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].wanted, "env node");
    assert_eq!(got[0].resolved, node);
}

#[test]
fn a_refused_shebang_executable_is_predicted_before_its_target() {
    // Strict mode can grant the script and `node` yet deny `/usr/bin/env`;
    // the kernel execs `env` first, so startup fails there.
    let d = tempfile::tempdir().unwrap();
    let npx = bin(d.path(), "npx", "#!/usr/bin/env node\n");
    let node = bin(d.path(), "node", "#!/bin/sh\n");
    let path = std::env::join_paths([d.path()]).unwrap();
    let got = predict_with(&profile("npx"), &no_bundle(), &path, |p| {
        p == npx || p == node
    });
    assert_eq!(
        got,
        vec![Blocked {
            server: "dc".into(),
            wanted: "#!/usr/bin/env".into(),
            resolved: PathBuf::from("/usr/bin/env"),
        }]
    );
}

#[test]
fn absolute_command_with_spaces_is_resolved_whole() {
    // The runtime spawns `entry.command` as one path (args live in
    // `entry.args`), so a space inside it is part of the path.
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("Application Support");
    std::fs::create_dir(&dir).unwrap();
    let node = bin(&dir, "node", "#!/bin/sh\n");
    let cmd = node.to_str().unwrap();
    let path = std::ffi::OsString::from("/definitely/not/here");
    let got = predict_with(&profile(cmd), &no_bundle(), &path, |_| false);
    assert_eq!(
        got,
        vec![Blocked {
            server: "dc".into(),
            wanted: cmd.into(),
            resolved: node,
        }]
    );
}

#[test]
fn unresolvable_command_is_left_to_the_runtime() {
    let path = std::ffi::OsString::from("/definitely/not/here");
    assert!(predict_with(&profile("nope"), &no_bundle(), &path, |_| false).is_empty());
}

#[test]
fn recorded_failures_only_for_the_live_pid() {
    let status = McpStatus {
        pid: 7,
        servers: vec![
            McpServerStatus {
                name: "ok".into(),
                tools: 2,
                error: None,
            },
            McpServerStatus {
                name: "dc".into(),
                tools: 0,
                error: Some("io: Operation not permitted (os error 1)".into()),
            },
        ],
    };
    let lines = format_recorded("mur", &status, 7);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("'dc'") && lines[0].contains("Operation not permitted"));
    assert!(
        format_recorded("mur", &status, 8).is_empty(),
        "stale boot ignored"
    );
}

fn no_bundle() -> PathBuf {
    PathBuf::from("/nonexistent/mur-mcp-server")
}

#[test]
fn bundled_mcp_server_is_judged_instead_of_a_path_copy() {
    // The runtime execs the bundled copy whenever it exists, so a refused
    // PATH copy of the same name must not be reported as blocked.
    let d = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let on_path = bin(d.path(), "mur-mcp-server", "#!/bin/sh\n");
    let bundled = bin(b.path(), "mur-mcp-server", "#!/bin/sh\n");
    let path = std::env::join_paths([d.path()]).unwrap();
    let got = predict_with(&profile("mur-mcp-server"), &bundled, &path, |p| {
        p == bundled
    });
    assert!(got.is_empty(), "{got:?}");
    let got = predict_with(&profile("mur-mcp-server"), &bundled, &path, |p| {
        p == on_path
    });
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].resolved, bundled);
}
