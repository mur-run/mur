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
    let got = predict_with(&profile("node"), &path, |_| false);
    assert_eq!(
        got,
        vec![Blocked {
            server: "dc".into(),
            wanted: "node".into(),
            resolved: node,
        }]
    );
    assert!(predict_with(&profile("node"), &path, |_| true).is_empty());
}

#[test]
fn env_shebang_interpreter_is_followed_one_hop() {
    let d = tempfile::tempdir().unwrap();
    let npx = bin(d.path(), "npx", "#!/usr/bin/env node\n");
    let node = bin(d.path(), "node", "#!/bin/sh\n");
    let path = std::env::join_paths([d.path()]).unwrap();
    let got = predict_with(&profile("npx"), &path, |p| p == npx);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].wanted, "env node");
    assert_eq!(got[0].resolved, node);
}

#[test]
fn unresolvable_command_is_left_to_the_runtime() {
    let path = std::ffi::OsString::from("/definitely/not/here");
    assert!(predict_with(&profile("nope"), &path, |_| false).is_empty());
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
