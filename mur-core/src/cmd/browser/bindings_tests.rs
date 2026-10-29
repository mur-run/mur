use std::fs;

use mur_browser::auth::{ProfileMeta, write_meta};
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};

use super::*;

fn agent(home: &Path, name: &str, yaml: &str) {
    let dir = home.join("agents").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("profile.yaml"), yaml).unwrap();
}

/// A valid agent profile whose only MCP server is the live `browser` entry.
fn live(name: &str, profile: Option<&str>, network: Option<McpServerNetwork>) -> String {
    let mut args = vec!["browser".to_string(), "live".to_string()];
    if let Some(p) = profile {
        args.extend(["--profile".to_string(), p.to_string()]);
    }
    let mut entry: McpServerEntry = serde_json::from_value(
        serde_json::json!({ "name": "browser", "command": "mur", "args": args }),
    )
    .unwrap();
    entry.network = network;
    let mut p = AgentProfile::default_for_tests();
    p.name = name.into();
    p.mcp_servers = vec![entry];
    serde_yaml_ng::to_string(&p).unwrap()
}

fn restricted() -> Option<McpServerNetwork> {
    Some(McpServerNetwork {
        mode: McpNetMode::Restricted,
        allow_hosts: vec!["*.deepseek.com".into()],
        ..Default::default()
    })
}

fn no_live(name: &str) -> String {
    let mut p = AgentProfile::default_for_tests();
    p.name = name.into();
    serde_yaml_ng::to_string(&p).unwrap()
}

fn saved(home: &Path, site: &str, url: &str) {
    fs::create_dir_all(paths::profile_dir(home, site)).unwrap();
    fs::write(paths::profile_state(home, site), b"encrypted").unwrap();
    write_meta(
        &paths::profile_meta(home, site),
        &ProfileMeta {
            url: url.into(),
            last_auth: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            earliest_cookie_expires: None,
            allow_domains: Vec::new(),
            last_refresh: None,
        },
    )
    .unwrap();
}

fn r(agent: &str, profile: &str, warning: Option<&str>) -> Row {
    Row {
        agent: agent.into(),
        profile: profile.into(),
        warning: warning.map(str::to_owned),
    }
}

#[test]
fn rows_cover_ok_unbound_uncovered_missing_and_unreadable() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    saved(h, "deepseek", "https://chat.deepseek.com/sign_in");
    saved(h, "gh", "https://github.com/login");
    agent(h, "a-ok", &live("a-ok", Some("deepseek"), restricted()));
    agent(h, "b-none", &live("b-none", None, restricted()));
    agent(h, "c-hosts", &live("c-hosts", Some("gh"), restricted()));
    agent(
        h,
        "d-missing",
        &live("d-missing", Some("gone"), restricted()),
    );
    agent(h, "e-broken", "mcp_servers: [unclosed");
    agent(h, "f-nolive", &no_live("f-nolive"));
    fs::create_dir_all(h.join("agents/g-empty-dir")).unwrap();

    let rows = rows(h);
    assert_eq!(
        rows[..4],
        [
            r("a-ok", "deepseek", None),
            r("b-none", "none", None),
            r("c-hosts", "gh", Some("allow_hosts lacks github.com")),
            r("d-missing", "gone", Some("profile missing")),
        ]
    );
    assert_eq!(rows[4].agent, "e-broken");
    assert_eq!(rows[4].profile, "?");
    assert!(
        rows[4]
            .warning
            .as_deref()
            .unwrap()
            .starts_with("agent profile unreadable: ")
    );
    assert_eq!(
        rows.len(),
        5,
        "agents without a live entry or profile.yaml are skipped"
    );
}

#[test]
fn unscoped_entry_is_flagged() {
    let t = tempfile::tempdir().unwrap();
    saved(t.path(), "deepseek", "https://chat.deepseek.com/");
    agent(t.path(), "x", &live("x", Some("deepseek"), None));
    assert_eq!(
        rows(t.path()),
        [r(
            "x",
            "deepseek",
            Some("network mode unset is not host-scoped")
        )]
    );
}

#[test]
fn incomplete_profile_is_flagged_before_hosts() {
    let t = tempfile::tempdir().unwrap();
    fs::create_dir_all(paths::profile_dir(t.path(), "half")).unwrap();
    agent(t.path(), "x", &live("x", Some("half"), restricted()));
    assert_eq!(rows(t.path()), [r("x", "half", Some("profile incomplete"))]);
}

#[test]
fn section_aligns_and_says_none_when_empty() {
    let t = tempfile::tempdir().unwrap();
    assert_eq!(section(t.path()), ["bindings: (none)"]);
    agent(t.path(), "longname", &live("longname", None, restricted()));
    agent(t.path(), "x", &live("x", Some("gone"), restricted()));
    assert_eq!(
        section(t.path()),
        [
            "bindings:",
            "  longname  → none",
            "  x         → gone  ⚠ profile missing",
        ]
    );
}
