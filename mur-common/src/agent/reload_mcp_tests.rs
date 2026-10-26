use super::super::{
    AgentProfile, McpAuth, McpNetMode, McpPackagePin, McpServerEntry, McpServerNetwork, ReloadDiff,
};

fn entry(name: &str) -> McpServerEntry {
    McpServerEntry {
        name: name.into(),
        command: "/opt/mcp/bin/server".into(),
        args: vec!["--a".into()],
        binary_sha256: Some("ab".repeat(32)),
        ..Default::default()
    }
}

fn profile(entries: Vec<McpServerEntry>) -> AgentProfile {
    let mut p = AgentProfile::default_for_tests();
    p.mcp_servers = entries;
    p
}

fn diff(old: Vec<McpServerEntry>, new: Vec<McpServerEntry>) -> ReloadDiff {
    AgentProfile::reload_diff(&profile(old), &profile(new))
}

#[test]
fn args_only_change_on_a_binary_entry_is_hot() {
    let mut b = entry("browser");
    b.args.push("--cdp".into());
    let d = diff(
        vec![entry("browser"), entry("other")],
        vec![b, entry("other")],
    );
    assert_eq!(d.hot, vec!["mcp_servers.browser"]);
    assert!(d.cold.is_empty(), "{d:?}");
}

#[test]
fn reordering_entries_is_not_a_change() {
    let d = diff(vec![entry("a"), entry("b")], vec![entry("b"), entry("a")]);
    assert!(d.is_empty(), "{d:?}");
}

#[test]
fn adding_or_removing_a_server_is_cold() {
    let add = diff(vec![entry("a")], vec![entry("a"), entry("b")]);
    assert_eq!(add.cold, vec!["mcp_servers"]);
    assert!(add.hot.is_empty());
    let remove = diff(vec![entry("a"), entry("b")], vec![entry("a")]);
    assert_eq!(remove.cold, vec!["mcp_servers"]);
}

#[test]
fn enable_toggle_is_cold() {
    let old = profile(vec![entry("a")]);
    let mut new = old.clone();
    new.disabled_mcp.push("a".into());
    let d = AgentProfile::reload_diff(&old, &new);
    assert_eq!(d.cold, vec!["disabled_mcp"]);
    assert!(d.hot.is_empty());
}

/// One field edit applied to a fresh entry.
type Edit = fn(&mut McpServerEntry);

#[test]
fn url_auth_mode_package_and_unknown_keys_are_cold() {
    let cases: Vec<(&str, Edit)> = vec![
        ("url", |e| e.url = Some("https://example.com/mcp".into())),
        ("auth", |e| {
            e.auth = Some(McpAuth::Bearer {
                token: crate::secret::SecretRef::Env("T".into()),
            })
        }),
        ("network", |e| {
            e.network = Some(McpServerNetwork {
                mode: McpNetMode::BroadAudited,
                ..Default::default()
            })
        }),
        ("package", |e| {
            e.package = Some(McpPackagePin {
                runner: "npm".into(),
                name: "p".into(),
                version: "1".into(),
                install_dir: "/opt/mcp/p".into(),
                lockfile_sha256: "cd".repeat(32),
                signatures_missing: None,
                provenance: None,
            })
        }),
        ("future_key", |e| {
            e.extra
                .insert("future_key".into(), serde_yaml_ng::Value::Bool(true));
        }),
    ];
    for (field, edit) in cases {
        let mut n = entry("a");
        edit(&mut n);
        let d = diff(vec![entry("a")], vec![n]);
        assert!(d.hot.is_empty(), "{field}: {d:?}");
        assert_eq!(d.cold, vec![format!("mcp_servers.a.{field}")], "{field}");
    }
}

#[test]
fn network_hosts_are_hot_but_mode_is_cold() {
    let scoped = |hosts: &[&str], mode| {
        let mut e = entry("a");
        e.network = Some(McpServerNetwork {
            mode,
            allow_hosts: hosts.iter().map(|h| h.to_string()).collect(),
            ..Default::default()
        });
        e
    };
    let hosts = diff(
        vec![scoped(&["a.example"], McpNetMode::Restricted)],
        vec![scoped(&["a.example", "b.example"], McpNetMode::Restricted)],
    );
    assert_eq!(hosts.hot, vec!["mcp_servers.a"]);
    let mode = diff(
        vec![scoped(&["a.example"], McpNetMode::Restricted)],
        vec![scoped(&["a.example"], McpNetMode::BroadAudited)],
    );
    assert_eq!(mode.cold, vec!["mcp_servers.a.network.mode"]);
    assert!(mode.hot.is_empty());
}

#[test]
fn mixed_hot_and_cold_change_makes_the_whole_entry_cold() {
    let mut n = entry("a");
    n.args.push("--b".into());
    n.url = Some("https://example.com/mcp".into());
    let old = profile(vec![entry("a")]);
    let new = profile(vec![n]);
    let d = AgentProfile::reload_diff(&old, &new);
    assert!(d.hot.is_empty(), "{d:?}");
    assert_eq!(d.cold, vec!["mcp_servers.a.url"]);
    // Nothing in the entry was applied, so only the cold path is left.
    let applied = old.with_hot_fields_from(&new);
    assert_eq!(applied.mcp_servers, old.mcp_servers);
    assert_eq!(AgentProfile::reload_diff(&applied, &new), d);
}

#[test]
fn hot_entry_is_applied_and_idempotent() {
    let mut n = entry("a");
    n.args = vec!["--live".into()];
    n.timeout_secs = Some(90);
    let old = profile(vec![entry("a"), entry("b")]);
    let new = profile(vec![n.clone(), entry("b")]);
    let applied = old.with_hot_fields_from(&new);
    assert_eq!(applied.mcp_servers[0], n);
    assert!(AgentProfile::reload_diff(&applied, &new).is_empty());
}

#[test]
fn interpreter_entry_args_change_is_cold() {
    let npx = |args: &[&str]| McpServerEntry {
        name: "a".into(),
        command: "npx".into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let d = diff(vec![npx(&["pkg@1"])], vec![npx(&["pkg@2"])]);
    assert!(d.hot.is_empty(), "{d:?}");
    assert_eq!(d.cold, vec!["mcp_servers.a.args"]);
}

#[test]
fn switching_to_an_interpreter_is_cold() {
    let mut n = entry("a");
    n.command = "uvx".into();
    let d = diff(vec![entry("a")], vec![n]);
    assert_eq!(d.cold, vec!["mcp_servers.a.command"]);
}

#[test]
fn command_change_without_a_pin_is_cold() {
    let mut n = entry("a");
    n.command = "/opt/mcp/bin/server2".into();
    n.binary_sha256 = None;
    let d = diff(vec![entry("a")], vec![n]);
    assert!(d.hot.is_empty(), "{d:?}");
    assert!(
        d.cold.contains(&"mcp_servers.a.command".to_string()),
        "{d:?}"
    );
}

#[test]
fn pinned_command_change_is_eligible() {
    let mut n = entry("a");
    n.command = "/opt/mcp/bin/server2".into();
    n.binary_sha256 = Some("cd".repeat(32));
    let d = diff(vec![entry("a")], vec![n]);
    assert_eq!(d.hot, vec!["mcp_servers.a"]);
}

#[test]
fn duplicate_names_fall_back_to_cold() {
    let mut n = entry("a");
    n.args.push("--x".into());
    let d = diff(vec![entry("a"), entry("a")], vec![entry("a"), n]);
    assert_eq!(d.cold, vec!["mcp_servers"]);
    assert!(d.hot.is_empty());
}

/// `boot` with `served` already live (an earlier hot reload) and `newer` on disk.
fn overlay(
    boot: &[McpServerEntry],
    served: &[McpServerEntry],
    newer: &[McpServerEntry],
) -> Vec<McpServerEntry> {
    profile(boot.to_vec())
        .with_hot_fields_over(served, &profile(newer.to_vec()))
        .mcp_servers
}

fn with_args(name: &str, args: &[&str]) -> McpServerEntry {
    let mut e = entry(name);
    e.args = args.iter().map(|s| s.to_string()).collect();
    e
}

#[test]
fn a_cold_edit_keeps_the_served_entry_not_boot() {
    let served = with_args("a", &["--new"]);
    let mut cold = served.clone();
    cold.url = Some("https://example.com/mcp".into());
    assert_eq!(
        overlay(&[entry("a")], std::slice::from_ref(&served), &[cold]),
        vec![served]
    );
}

#[test]
fn a_cold_network_edit_keeps_the_served_host_restriction() {
    let net = |hosts: &[&str], mode| McpServerNetwork {
        mode,
        deny_hosts: hosts.iter().map(|h| h.to_string()).collect(),
        ..Default::default()
    };
    let mut boot = entry("a");
    boot.network = Some(net(&[], McpNetMode::Restricted));
    let mut served = boot.clone();
    served.network = Some(net(&["evil.example"], McpNetMode::Restricted));
    let mut cold = served.clone();
    cold.network = Some(net(&["evil.example"], McpNetMode::BroadAudited));
    assert_eq!(
        overlay(&[boot], std::slice::from_ref(&served), &[cold]),
        vec![served]
    );
}

#[test]
fn adding_or_removing_a_server_keeps_served_hot_entries() {
    let served = vec![with_args("a", &["--new"]), entry("b")];
    let added = vec![with_args("a", &["--new"]), entry("b"), entry("c")];
    let boot = vec![entry("a"), entry("b")];
    assert_eq!(overlay(&boot, &served, &added), served);
    // The set changed, so nothing in it is hot — not even `a`'s new args.
    let removed = vec![with_args("a", &["--newer"])];
    assert_eq!(overlay(&boot, &served, &removed), served);
}

#[test]
fn reverting_to_the_boot_value_is_applied() {
    let served = with_args("a", &["--new"]);
    assert_eq!(
        overlay(&[entry("a")], &[served], &[entry("a")]),
        vec![entry("a")]
    );
}
