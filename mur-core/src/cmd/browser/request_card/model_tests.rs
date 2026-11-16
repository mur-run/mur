use mur_browser_request::Site;
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork, NetworkOutboundMode};

use super::*;
use crate::cmd::browser::bind_plan::BoundDomains;

fn entry(
    mode: Option<McpNetMode>,
    allow: &[&str],
    deny: &[&str],
    bound: &[&str],
) -> McpServerEntry {
    let mut args = vec!["browser".to_string(), "live".to_string()];
    for p in bound {
        args.extend(["--profile".to_string(), p.to_string()]);
    }
    let mut e: McpServerEntry = serde_json::from_value(
        serde_json::json!({ "name": "browser", "command": "mur", "args": args }),
    )
    .unwrap();
    e.network = mode.map(|mode| McpServerNetwork {
        mode,
        allow_hosts: allow.iter().map(|s| s.to_string()).collect(),
        deny_hosts: deny.iter().map(|s| s.to_string()).collect(),
        authorization: None,
    });
    e
}

fn restricted(allow: &[&str], bound: &[&str]) -> McpServerEntry {
    entry(Some(McpNetMode::Restricted), allow, &[], bound)
}

fn p(name: &str, domains: &[&str]) -> Profile {
    Profile {
        name: name.into(),
        domains: domains.iter().map(|s| s.to_string()).collect(),
    }
}

fn site(s: &str) -> Site {
    Site::parse(s).unwrap()
}

fn disjoint(_: &str) -> Option<Vec<String>> {
    Some(Vec::new())
}

fn nobody(_: &str) -> Vec<String> {
    Vec::new()
}

fn again() -> LoginAgain {
    LoginAgain {
        name: "again".into(),
        exists: false,
    }
}

fn card_with(
    s: &str,
    login: bool,
    entry: Option<&McpServerEntry>,
    profiles: &[Profile],
    bound: &BoundDomains<'_>,
) -> Card {
    build(
        &site(s),
        login,
        again(),
        &Inputs {
            entry,
            profiles,
            bound,
            shared: &nobody,
            port: None,
        },
    )
}

fn card(s: &str, login: bool, entry: Option<&McpServerEntry>, profiles: &[Profile]) -> Card {
    card_with(s, login, entry, profiles, &disjoint)
}

fn rows(c: Card) -> Vec<ProfileRow> {
    match c {
        Card::Login { rows, .. } => rows,
        other => panic!("not a login card: {other:?}"),
    }
}

fn offer_of(c: Card) -> Offer {
    rows(c).remove(0).offer
}

fn hosts(h: &[&str]) -> Vec<String> {
    h.iter().map(|s| s.to_string()).collect()
}

#[test]
fn no_live_entry_is_its_own_card() {
    assert_eq!(
        card("gitea.example", true, None, &[p("g", &["gitea.example"])]),
        Card::NoLiveEntry
    );
    assert_eq!(card("gitea.example", false, None, &[]), Card::NoLiveEntry);
}

#[test]
fn bind_is_offered_and_the_row_carries_its_domains() {
    let e = restricted(&["gitea.example"], &[]);
    let r = rows(card(
        "gitea.example",
        true,
        Some(&e),
        &[p("g", &["gitea.example"])],
    ))
    .remove(0);
    assert_eq!(
        (r.offer, r.domains),
        (Offer::Offered, hosts(&["gitea.example"]))
    );
}

#[test]
fn a_profile_already_bound_is_already_set_up() {
    let e = restricted(&["gitea.example"], &["g"]);
    let c = card(
        "gitea.example",
        true,
        Some(&e),
        &[p("g", &["gitea.example"])],
    );
    assert_eq!(offer_of(c), Offer::AlreadyBound);
}

/// Council B2: being attached does not outrank the planner.
#[test]
fn an_already_bound_profile_the_entry_blocks_is_disabled() {
    let denied = entry(
        Some(McpNetMode::BroadAudited),
        &[],
        &["gitea.example"],
        &["g"],
    );
    let off = entry(Some(McpNetMode::Off), &[], &[], &["g"]);
    for e in [denied, off] {
        let c = card(
            "gitea.example",
            true,
            Some(&e),
            &[p("g", &["gitea.example"])],
        );
        assert!(matches!(offer_of(c), Offer::Disabled { .. }), "{e:?}");
    }
}

/// Council B1: a profile that matches by subdomain still needs the request
/// host in `allow_hosts`; the row adds it, it does not claim "set up".
#[test]
fn a_subdomain_request_under_restricted_adds_the_host() {
    let e = restricted(&["deepseek.com"], &[]);
    let c = card(
        "chat.deepseek.com",
        true,
        Some(&e),
        &[p("ds", &["deepseek.com"])],
    );
    assert_eq!(
        offer_of(c),
        Offer::AddsHosts {
            missing: hosts(&["chat.deepseek.com"])
        }
    );

    let bound = restricted(&["deepseek.com"], &["ds"]);
    let c = card(
        "chat.deepseek.com",
        true,
        Some(&bound),
        &[p("ds", &["deepseek.com"])],
    );
    assert_eq!(
        offer_of(c),
        Offer::AddsHosts {
            missing: hosts(&["chat.deepseek.com"])
        }
    );
}

/// D4, council N5: the row names every profile a replace drops.
#[test]
fn overlap_is_replace_only_and_names_everything_it_drops() {
    let e = restricted(&["gitea.example"], &["old", "other"]);
    let bound = |n: &str| match n {
        "old" => Some(hosts(&["gitea.example"])),
        _ => Some(hosts(&["elsewhere.invalid"])),
    };
    let e2 = restricted(&["gitea.example", "elsewhere.invalid"], &["old", "other"]);
    for e in [&e, &e2] {
        let c = card_with(
            "gitea.example",
            true,
            Some(e),
            &[p("g", &["gitea.example"])],
            &bound,
        );
        let Offer::ReplaceOnly {
            with,
            domains,
            drops,
            ..
        } = offer_of(c)
        else {
            panic!("{e:?}")
        };
        assert_eq!(
            (with.as_str(), domains, drops),
            ("old", hosts(&["gitea.example"]), hosts(&["old", "other"]))
        );
    }
}

#[test]
fn needs_hosts_names_the_missing_hosts() {
    let e = restricted(&["other.example"], &[]);
    let c = card(
        "gitea.example",
        true,
        Some(&e),
        &[p("g", &["gitea.example"])],
    );
    let o = offer_of(c);
    assert_eq!(
        o,
        Offer::AddsHosts {
            missing: hosts(&["gitea.example"])
        }
    );
    assert_eq!(o.approved(), hosts(&["gitea.example"]).as_slice());
}

#[test]
fn denied_is_disabled_and_names_the_host() {
    let e = entry(Some(McpNetMode::BroadAudited), &[], &["gitea.example"], &[]);
    let c = card(
        "gitea.example",
        true,
        Some(&e),
        &[p("g", &["gitea.example"])],
    );
    let Offer::Disabled { why } = offer_of(c) else {
        panic!()
    };
    assert!(why.contains("deny_hosts blocks gitea.example"), "{why}");
}

#[test]
fn unscoped_modes_are_disabled_for_a_login_and_name_the_mode() {
    for (mode, word) in [
        (None, "unset"),
        (Some(McpNetMode::Inherit), "inherit"),
        (Some(McpNetMode::Off), "off"),
    ] {
        let e = entry(mode, &[], &[], &[]);
        let c = card(
            "gitea.example",
            true,
            Some(&e),
            &[p("g", &["gitea.example"])],
        );
        let Offer::Disabled { why } = offer_of(c) else {
            panic!("{word}")
        };
        assert!(
            why.contains(word) && why.contains("not host-scoped"),
            "{why}"
        );
    }
}

/// D1: a bare profile domain covers its subdomains, label-aligned; other
/// hosts are not listed at all.
#[test]
fn match_covers_subdomains_and_nothing_else() {
    let e = entry(Some(McpNetMode::BroadAudited), &[], &[], &[]);
    let profiles = [
        p("ds", &["deepseek.com"]),
        p("evil", &["seek.com"]),
        p("gh", &["github.com"]),
    ];
    let names: Vec<String> = rows(card("chat.deepseek.com", true, Some(&e), &profiles))
        .into_iter()
        .map(|r| r.name)
        .collect();
    assert_eq!(names, vec!["ds"]);
}

#[test]
fn shared_lists_the_other_agents() {
    let e = restricted(&["gitea.example"], &[]);
    let shared = |n: &str| {
        if n == "g" {
            vec!["reviewer".to_string()]
        } else {
            vec![]
        }
    };
    let c = build(
        &site("gitea.example"),
        true,
        LoginAgain {
            name: "gitea.example".into(),
            exists: true,
        },
        &Inputs {
            entry: Some(&e),
            profiles: &[p("g", &["gitea.example"])],
            bound: &disjoint,
            shared: &shared,
            port: Some(3000),
        },
    );
    let Card::Login {
        rows,
        login_again,
        port,
    } = c
    else {
        panic!()
    };
    assert_eq!(rows[0].shared_with, vec!["reviewer"]);
    assert_eq!(
        (login_again.name.as_str(), login_again.exists, port),
        ("gitea.example", true, Some(3000))
    );
}

#[test]
fn port_step_only_for_a_closed_non_web_non_zero_port_under_restricted() {
    let r = NetworkOutboundMode::Restricted;
    assert_eq!(
        port_step(&site("http://g.example:3000"), r, &[]),
        Some(3000)
    );
    assert_eq!(port_step(&site("http://g.example:3000"), r, &[3000]), None);
    assert_eq!(port_step(&site("http://g.example:8080"), r, &[]), None);
    assert_eq!(port_step(&site("http://g.example:0"), r, &[]), None);
    assert_eq!(port_step(&site("https://g.example"), r, &[]), None);
    assert_eq!(
        port_step(
            &site("http://g.example:3000"),
            NetworkOutboundMode::Unrestricted,
            &[]
        ),
        None
    );
}

/// D7: `inherit` and unset are host-open, so they are not "not available".
#[test]
fn a_no_login_request_gets_a_reach_card() {
    let reach = |e: &McpServerEntry| match card("gitea.example", false, Some(e), &[]) {
        Card::Reach { reach, .. } => reach,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        reach(&restricted(&["gitea.example"], &["g"])),
        Reach::Allowed
    );
    assert_eq!(
        reach(&restricted(&["other.example"], &[])),
        Reach::NeedsHost {
            missing: hosts(&["gitea.example"])
        }
    );
    assert_eq!(
        reach(&entry(None, &[], &[], &[])),
        Reach::Unscoped {
            mode: "unset".into()
        }
    );
    assert_eq!(
        reach(&entry(Some(McpNetMode::Inherit), &[], &[], &[])),
        Reach::Unscoped {
            mode: "inherit".into()
        }
    );
    assert!(matches!(
        reach(&entry(Some(McpNetMode::Off), &[], &[], &[])),
        Reach::Disabled { .. }
    ));
    let denied = entry(Some(McpNetMode::BroadAudited), &[], &["gitea.example"], &[]);
    assert!(matches!(reach(&denied), Reach::Disabled { .. }));
}

#[test]
fn add_hosts_touches_only_a_restricted_entry_and_keeps_order() {
    let mut e = restricted(&["b.example"], &[]);
    add_hosts(&mut e, &hosts(&["a.example", "b.example"]));
    assert_eq!(
        e.network.unwrap().allow_hosts,
        hosts(&["b.example", "a.example"])
    );
    let mut broad = entry(Some(McpNetMode::BroadAudited), &[], &[], &[]);
    add_hosts(&mut broad, &hosts(&["a.example"]));
    assert!(broad.network.unwrap().allow_hosts.is_empty());
}
