use super::*;
use crate::cmd::code_nav::serena_install::serena_binary_path_in;
use mur_agent_runtime::mcp::serena::verify_entries;
use tempfile::TempDir;

struct Fx {
    _tmp: TempDir,
    record: Record,
    repo: PathBuf,
}

/// A fake pinned install (entry point only) and a repo directory.
fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tools/serena/pin");
    let bin = serena_binary_path_in(&dir);
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    Fx {
        record: Record::for_dir(&dir),
        repo,
        _tmp: tmp,
    }
}

fn profile() -> AgentProfile {
    AgentProfile::default_for_tests()
}

#[test]
fn entry_has_the_layer_b_shape() {
    let f = fx();
    let e = build(&f.record, &f.repo).unwrap();
    assert_eq!(e.name, "serena");
    assert_eq!(e.command, f.record.bin.to_str().unwrap());
    assert_eq!(e.args, ["start-mcp-server", "--transport", "stdio"]);
    assert_eq!(e.kind, Some(McpServerKind::Serena));
    assert_eq!(e.project, Some(std::fs::canonicalize(&f.repo).unwrap()));
    let want = crate::cmd::agent_mcp_pin::compute_binary_sha256(&f.record.bin).unwrap();
    assert_eq!(e.binary_sha256.as_deref(), Some(want.as_str()));
}

#[test]
fn runtime_owned_flags_are_not_written() {
    let f = fx();
    let e = build(&f.record, &f.repo).unwrap();
    for flag in [
        "--project",
        "--enable-web-dashboard",
        "--open-web-dashboard",
    ] {
        assert!(!e.args.iter().any(|a| a == flag), "{flag} in {:?}", e.args);
    }
}

#[test]
fn relative_project_is_made_absolute() {
    let f = fx();
    let cwd = std::env::current_dir().unwrap();
    let e = build(&f.record, Path::new(".")).unwrap();
    assert_eq!(e.project, Some(std::fs::canonicalize(cwd).unwrap()));
}

#[test]
fn missing_project_or_file_is_refused() {
    let f = fx();
    let e = build(&f.record, &f.repo.join("nope")).unwrap_err();
    assert!(e.to_string().contains("does not exist"), "{e:#}");
    let file = f.repo.join("a.txt");
    std::fs::write(&file, b"x").unwrap();
    let e = build(&f.record, &file).unwrap_err();
    assert!(e.to_string().contains("not a directory"), "{e:#}");
}

#[test]
fn missing_install_is_refused() {
    let f = fx();
    std::fs::remove_file(&f.record.bin).unwrap();
    assert!(build(&f.record, &f.repo).is_err());
}

#[test]
fn the_runtime_project_gate_accepts_the_entry() {
    // `verify_entries` checks `project` before the config preflight; a
    // missing config must fail on the preflight, proving `project` passed.
    let f = fx();
    let e = build(&f.record, &f.repo).unwrap();
    let home = f.repo.parent().unwrap().join("agent");
    let err = verify_entries(&[e], &home).unwrap_err().to_string();
    assert!(!err.contains("`project` is"), "{err}");
}

#[test]
fn upsert_adds_then_is_idempotent() {
    let f = fx();
    let mut p = profile();
    let e = build(&f.record, &f.repo).unwrap();
    assert_eq!(upsert(&mut p, e.clone()).unwrap(), Change::Added);
    let again = McpServerEntry {
        installed_at: Some(chrono::Utc::now() + chrono::Duration::seconds(5)),
        ..e.clone()
    };
    assert_eq!(upsert(&mut p, again).unwrap(), Change::Unchanged);
    assert_eq!(p.mcp_servers.len(), 1);
    assert_eq!(p.mcp_servers[0].installed_at, e.installed_at);
    let spawn = &p.entitlements.processes.spawn.allowed;
    assert_eq!(spawn.iter().filter(|a| **a == e.command).count(), 1);
}

#[test]
fn upsert_retargets_the_project() {
    let f = fx();
    let mut p = profile();
    upsert(&mut p, build(&f.record, &f.repo).unwrap()).unwrap();
    let other = f.repo.parent().unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let e = build(&f.record, &other).unwrap();
    assert_eq!(upsert(&mut p, e).unwrap(), Change::Updated);
    assert_eq!(p.mcp_servers.len(), 1);
    assert_eq!(
        p.mcp_servers[0].project,
        Some(std::fs::canonicalize(other).unwrap())
    );
}

#[test]
fn upsert_never_overwrites_a_users_own_server() {
    let f = fx();
    let mut p = profile();
    p.mcp_servers.push(McpServerEntry {
        name: "serena".into(),
        command: "/usr/local/bin/serena".into(),
        ..Default::default()
    });
    let err = upsert(&mut p, build(&f.record, &f.repo).unwrap()).unwrap_err();
    assert!(err.to_string().contains("not kind: serena"), "{err}");
    assert_eq!(p.mcp_servers[0].command, "/usr/local/bin/serena");
    assert_eq!(p.mcp_servers[0].kind, None);
}

/// The pre-install gate (`setup::run` calls it before any uv run or write)
/// refuses exactly what `upsert` would, and passes a free or own slot.
#[test]
fn check_slot_matches_upserts_refusal() {
    let f = fx();
    let mut p = profile();
    assert!(check_slot(&p, ENTRY_NAME).is_ok(), "free slot");
    upsert(&mut p, build(&f.record, &f.repo).unwrap()).unwrap();
    assert!(
        check_slot(&p, ENTRY_NAME).is_ok(),
        "our own kind: serena slot"
    );

    let mut q = profile();
    q.mcp_servers.push(McpServerEntry {
        name: "serena".into(),
        command: "uvx".into(),
        ..Default::default()
    });
    let err = check_slot(&q, ENTRY_NAME).unwrap_err().to_string();
    assert!(err.contains("not kind: serena"), "{err}");
    assert!(err.contains("mur agent mcp remove"), "{err}");
}

#[test]
fn entry_round_trips_through_profile_yaml() {
    let f = fx();
    let mut p = profile();
    upsert(&mut p, build(&f.record, &f.repo).unwrap()).unwrap();
    let y = serde_yaml_ng::to_string(&p).unwrap();
    assert!(y.contains("kind: serena"), "{y}");
    let back: AgentProfile = serde_yaml_ng::from_str(&y).unwrap();
    assert_eq!(back.mcp_servers, p.mcp_servers);
}
