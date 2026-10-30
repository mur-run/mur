use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};

use super::*;

fn entry(
    mode: Option<McpNetMode>,
    allow: &[&str],
    deny: &[&str],
    profile: Option<&str>,
) -> McpServerEntry {
    let mut args = vec!["browser".to_string(), "live".to_string()];
    if let Some(p) = profile {
        args.extend(["--profile".to_string(), p.to_string()]);
    }
    let yaml = serde_json::json!({ "name": "browser", "command": "mur", "args": args });
    let mut e: McpServerEntry = serde_json::from_value(yaml).unwrap();
    e.network = mode.map(|mode| McpServerNetwork {
        mode,
        allow_hosts: allow.iter().map(|s| s.to_string()).collect(),
        deny_hosts: deny.iter().map(|s| s.to_string()).collect(),
        authorization: None,
    });
    e
}

fn d(hosts: &[&str]) -> Vec<String> {
    hosts.iter().map(|s| s.to_string()).collect()
}

#[test]
fn no_entry_is_no_live_entry() {
    assert_eq!(
        plan_bind(None, "ds", &d(&["deepseek.com"])),
        BindPlan::NoLiveEntry
    );
}

#[test]
fn restricted_binds_only_when_every_domain_passes_the_proxy_matcher() {
    let e = entry(
        Some(McpNetMode::Restricted),
        &["*.deepseek.com", "gitea.example"],
        &[],
        None,
    );
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["chat.deepseek.com", "deepseek.com"])),
        BindPlan::Bind { replaces: None }
    );
}

#[test]
fn restricted_exact_subdomain_does_not_cover_the_parent() {
    // The recommender's two-way overlap would accept this; the proxy would
    // then block api.deepseek.com. The planner follows the proxy.
    let e = entry(
        Some(McpNetMode::Restricted),
        &["chat.deepseek.com"],
        &[],
        None,
    );
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["deepseek.com"])),
        BindPlan::NeedsHosts {
            current: d(&["chat.deepseek.com"]),
            missing: d(&["deepseek.com"])
        }
    );
}

#[test]
fn restricted_with_empty_allow_hosts_needs_every_domain() {
    let e = entry(Some(McpNetMode::Restricted), &[], &[], None);
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["deepseek.com"])),
        BindPlan::NeedsHosts {
            current: vec![],
            missing: d(&["deepseek.com"])
        }
    );
}

#[test]
fn broad_audited_with_empty_allow_hosts_binds() {
    // The research-collapse case: `--research` leaves allow_hosts empty. Reading
    // allow_hosts alone would call this "allows nothing" and suggest a two-host
    // `/browser live` that silently drops research mode.
    let e = entry(Some(McpNetMode::BroadAudited), &[], &[], None);
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["deepseek.com"])),
        BindPlan::Bind { replaces: None }
    );
}

#[test]
fn broad_audited_refuses_a_denied_domain() {
    let e = entry(
        Some(McpNetMode::BroadAudited),
        &[],
        &["*.deepseek.com"],
        None,
    );
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["chat.deepseek.com", "example.org"])),
        BindPlan::Denied {
            hosts: d(&["chat.deepseek.com"])
        }
    );
}

#[test]
fn unscoped_modes_are_refused() {
    for mode in [None, Some(McpNetMode::Inherit), Some(McpNetMode::Off)] {
        let e = entry(mode, &["deepseek.com"], &[], None);
        assert_eq!(
            plan_bind(Some(&e), "ds", &d(&["deepseek.com"])),
            BindPlan::NotHostScoped { mode },
            "{mode:?}"
        );
    }
}

#[test]
fn bind_names_the_profile_it_replaces_but_not_itself() {
    let e = entry(
        Some(McpNetMode::Restricted),
        &["deepseek.com"],
        &[],
        Some("old"),
    );
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["deepseek.com"])),
        BindPlan::Bind {
            replaces: Some("old".into())
        }
    );
    assert_eq!(
        plan_bind(Some(&e), "old", &d(&["deepseek.com"])),
        BindPlan::Bind { replaces: None }
    );
}

#[test]
fn profile_domains_default_to_login_host_and_normalize() {
    let mut meta = ProfileMeta {
        url: "https://Chat.DeepSeek.com./sign_in".into(),
        last_auth: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        earliest_cookie_expires: None,
        allow_domains: Vec::new(),
        last_refresh: None,
    };
    assert_eq!(profile_domains(&meta), Some(d(&["chat.deepseek.com"])));
    meta.allow_domains = d(&["DeepSeek.com"]);
    assert_eq!(profile_domains(&meta), Some(d(&["deepseek.com"])));
    meta.allow_domains.clear();
    meta.url = "not a url".into();
    assert_eq!(profile_domains(&meta), None);
}

#[test]
fn warnings_are_none_only_for_bind() {
    assert_eq!(warning(&BindPlan::Bind { replaces: None }), None);
    assert_eq!(
        warning(&BindPlan::NeedsHosts {
            current: vec![],
            missing: d(&["a.test"])
        })
        .unwrap(),
        "allow_hosts lacks a.test"
    );
    assert_eq!(
        warning(&BindPlan::NotHostScoped {
            mode: Some(McpNetMode::BroadAudited)
        })
        .unwrap(),
        "network mode broadaudited is not host-scoped"
    );
}
