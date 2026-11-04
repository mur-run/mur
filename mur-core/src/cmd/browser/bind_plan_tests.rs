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

/// Bound-profile lookup for tests: every already-bound profile is for a host
/// nothing else uses, so the plans below exercise the policy checks alone.
fn disjoint(_: &str) -> Option<Vec<String>> {
    Some(vec!["elsewhere.invalid".into()])
}

fn add(entry: Option<&McpServerEntry>, site: &str, domains: &[String]) -> BindPlan {
    plan_bind(entry, site, domains, &disjoint, false)
}

fn bind_none() -> BindPlan {
    BindPlan::Bind {
        alongside: Vec::new(),
        replaces: Vec::new(),
    }
}

#[test]
fn no_entry_is_no_live_entry() {
    assert_eq!(
        add(None, "ds", &d(&["deepseek.com"])),
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
        add(Some(&e), "ds", &d(&["chat.deepseek.com", "deepseek.com"])),
        bind_none()
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
        add(Some(&e), "ds", &d(&["deepseek.com"])),
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
        add(Some(&e), "ds", &d(&["deepseek.com"])),
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
    assert_eq!(add(Some(&e), "ds", &d(&["deepseek.com"])), bind_none());
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
        add(Some(&e), "ds", &d(&["chat.deepseek.com", "example.org"])),
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
            add(Some(&e), "ds", &d(&["deepseek.com"])),
            BindPlan::NotHostScoped { mode },
            "{mode:?}"
        );
    }
}

#[test]
fn disjoint_profile_is_added_alongside_the_bound_one_but_never_itself() {
    let e = entry(
        Some(McpNetMode::Restricted),
        &["deepseek.com", "gitea.example"],
        &[],
        Some("gitea"),
    );
    let gitea = |s: &str| (s == "gitea").then(|| d(&["gitea.example"]));
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["deepseek.com"]), &gitea, false),
        BindPlan::Bind {
            alongside: vec!["gitea".into()],
            replaces: Vec::new(),
        }
    );
    assert_eq!(
        plan_bind(Some(&e), "gitea", &d(&["gitea.example"]), &gitea, false),
        bind_none()
    );
}

#[test]
fn replace_drops_every_bound_profile_instead_of_adding() {
    let mut e = entry(
        Some(McpNetMode::Restricted),
        &["deepseek.com", "gitea.example", "x.com"],
        &[],
        Some("gitea"),
    );
    e.args.extend(["--profile".to_string(), "x".to_string()]);
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["deepseek.com"]), &disjoint, true),
        BindPlan::Bind {
            alongside: Vec::new(),
            replaces: vec!["gitea".into(), "x".into()],
        }
    );
}

/// Two logins for one host cannot share a session: the merge would keep one
/// cookie and silently pick an identity. Either direction of subdomain counts.
#[test]
fn overlapping_domains_refuse_the_add_and_name_the_shared_host() {
    let e = entry(
        Some(McpNetMode::Restricted),
        &["*.deepseek.com", "deepseek.com"],
        &[],
        Some("old-ds"),
    );
    let old = |s: &str| (s == "old-ds").then(|| d(&["deepseek.com"]));
    assert_eq!(
        plan_bind(Some(&e), "ds", &d(&["chat.deepseek.com"]), &old, false),
        BindPlan::Overlaps {
            with: "old-ds".into(),
            domains: d(&["chat.deepseek.com"]),
        }
    );
    let old = |s: &str| (s == "old-ds").then(|| d(&["chat.deepseek.com"]));
    assert!(matches!(
        plan_bind(Some(&e), "ds", &d(&["deepseek.com"]), &old, false),
        BindPlan::Overlaps { .. }
    ));
    // `--replace` is the way past an overlap.
    assert!(matches!(
        plan_bind(Some(&e), "ds", &d(&["deepseek.com"]), &old, true),
        BindPlan::Bind { .. }
    ));
}

/// A bound profile whose metadata is gone cannot be proven disjoint, so it
/// counts as overlapping everything rather than merging blind.
#[test]
fn unreadable_bound_profile_counts_as_an_overlap() {
    let e = entry(Some(McpNetMode::Restricted), &["x.com"], &[], Some("ghost"));
    let gone = |_: &str| None;
    assert_eq!(
        plan_bind(Some(&e), "x", &d(&["x.com"]), &gone, false),
        BindPlan::Overlaps {
            with: "ghost".into(),
            domains: d(&["x.com"]),
        }
    );
}

/// The policy check comes first: an uncovered host is reported as such even
/// when the profile would also overlap.
#[test]
fn needs_hosts_is_reported_before_overlap() {
    let e = entry(Some(McpNetMode::Restricted), &["a.com"], &[], Some("old"));
    let gone = |_: &str| None;
    assert!(matches!(
        plan_bind(Some(&e), "x", &d(&["x.com"]), &gone, false),
        BindPlan::NeedsHosts { .. }
    ));
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
    assert_eq!(warning(&bind_none()), None);
    assert_eq!(
        warning(&BindPlan::Overlaps {
            with: "old".into(),
            domains: d(&["a.x.com", "x.com"]),
        }),
        Some("shares a.x.com, x.com with bound profile \"old\"".into())
    );
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

/// #2263: the policy check covers the union of every retained profile's
/// domains plus the incoming one. A retained profile whose hosts the entry
/// no longer allows blocks adding even a disjoint, fully covered profile.
#[test]
fn restricted_refuses_when_a_retained_profile_is_no_longer_covered() {
    let e = entry(Some(McpNetMode::Restricted), &["b.com"], &[], Some("a"));
    let bound = |site: &str| (site == "a").then(|| d(&["a.com"]));
    assert_eq!(
        plan_bind(Some(&e), "b", &d(&["b.com"]), &bound, false),
        BindPlan::NeedsHosts {
            current: d(&["b.com"]),
            missing: d(&["a.com"]),
        }
    );
}

/// With `--replace` the retained profiles are dropped, so only the
/// replacement has to be covered.
#[test]
fn replace_checks_only_the_replacement_against_the_policy() {
    let e = entry(Some(McpNetMode::Restricted), &["b.com"], &[], Some("a"));
    let bound = |site: &str| (site == "a").then(|| d(&["a.com"]));
    assert_eq!(
        plan_bind(Some(&e), "b", &d(&["b.com"]), &bound, true),
        BindPlan::Bind {
            alongside: Vec::new(),
            replaces: d(&["a"]),
        }
    );
}

/// Broad-audited mirrors restricted: a retained profile on a denied host
/// blocks the add.
#[test]
fn broad_audited_refuses_when_a_retained_profile_is_denied() {
    let e = entry(Some(McpNetMode::BroadAudited), &[], &["a.com"], Some("a"));
    let bound = |site: &str| (site == "a").then(|| d(&["a.com"]));
    assert_eq!(
        plan_bind(Some(&e), "b", &d(&["b.com"]), &bound, false),
        BindPlan::Denied {
            hosts: d(&["a.com"])
        }
    );
}
