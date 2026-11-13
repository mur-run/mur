use super::*;
use crate::merge::{Part, merge_states};

const SECRET_COOKIE: &str = "SENTINEL-cookie-value-7f3a";
const SECRET_LS: &str = "SENTINEL-localstorage-value-91be";

fn doc(cookies: &[(&str, &str)], origins: &[&str]) -> Vec<u8> {
    let cookies: Vec<Value> = cookies
        .iter()
        .map(|(name, domain)| json!({"name": name, "value": SECRET_COOKIE, "domain": domain, "path": "/"}))
        .collect();
    let origins: Vec<Value> = origins
        .iter()
        .map(|o| json!({"origin": o, "localStorage": [{"name": "t", "value": SECRET_LS}]}))
        .collect();
    serde_json::to_vec(&json!({"cookies": cookies, "origins": origins})).unwrap()
}

fn d(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn part(s: &Split, site: &str) -> Value {
    serde_json::from_slice(&s.parts[site]).unwrap()
}

#[test]
fn cookies_and_origins_both_go_to_their_own_profile() {
    let (a, b) = (d(&["a.example"]), d(&["b.example"]));
    let t = [
        Target {
            site: "a",
            domains: &a,
        },
        Target {
            site: "b",
            domains: &b,
        },
    ];
    let s = split_state(
        &doc(
            &[("sa", ".a.example"), ("sb", "www.b.example")],
            &["https://app.a.example", "https://b.example"],
        ),
        &t,
    )
    .unwrap();
    let pa = part(&s, "a");
    let pb = part(&s, "b");
    assert_eq!(pa["cookies"][0]["name"], "sa");
    assert_eq!(pa["origins"][0]["origin"], "https://app.a.example");
    assert_eq!(pb["cookies"][0]["name"], "sb");
    assert_eq!(pb["origins"][0]["origin"], "https://b.example");
    assert!(s.overlaps.is_empty());
    assert_eq!(s.unmatched, 0);
}

#[test]
fn host_only_and_leading_dot_cookies_match_alike_and_keep_their_domain() {
    let a = d(&["a.example"]);
    let t = [Target {
        site: "a",
        domains: &a,
    }];
    let s = split_state(&doc(&[("h", "a.example"), ("dot", ".a.example")], &[]), &t).unwrap();
    let pa = part(&s, "a");
    assert_eq!(pa["cookies"][0]["domain"], "a.example");
    assert_eq!(pa["cookies"][1]["domain"], ".a.example");
}

#[test]
fn scheme_and_port_never_pick_a_profile_and_the_origin_is_kept_whole() {
    let a = d(&["a.example"]);
    let t = [Target {
        site: "a",
        domains: &a,
    }];
    let s = split_state(
        &doc(&[], &["http://a.example", "https://a.example:8443"]),
        &t,
    )
    .unwrap();
    let pa = part(&s, "a");
    assert_eq!(pa["origins"][1]["origin"], "https://a.example:8443");
    assert_eq!(pa["origins"].as_array().unwrap().len(), 2);
}

#[test]
fn a_bracketed_ipv6_origin_matches_by_its_address() {
    let a = d(&["::1"]);
    let t = [Target {
        site: "a",
        domains: &a,
    }];
    let s = split_state(&doc(&[], &["http://[::1]:3000"]), &t).unwrap();
    assert_eq!(part(&s, "a")["origins"].as_array().unwrap().len(), 1);
}

#[test]
fn an_item_matching_two_profiles_goes_to_neither_and_names_both() {
    // Both bound to a shared identity provider.
    let (a, b) = (
        d(&["a.example", "idp.example"]),
        d(&["b.example", "idp.example"]),
    );
    let t = [
        Target {
            site: "a",
            domains: &a,
        },
        Target {
            site: "b",
            domains: &b,
        },
    ];
    let s = split_state(
        &doc(
            &[("sso", ".idp.example"), ("sa", "a.example")],
            &["https://login.idp.example"],
        ),
        &t,
    )
    .unwrap();
    assert_eq!(part(&s, "a")["cookies"].as_array().unwrap().len(), 1);
    assert_eq!(part(&s, "b")["cookies"].as_array().unwrap().len(), 0);
    assert_eq!(part(&s, "b")["origins"].as_array().unwrap().len(), 0);
    assert_eq!(s.overlaps.len(), 2);
    let c = &s.overlaps[0];
    assert_eq!(
        (c.host.as_str(), c.kind, c.name.as_deref()),
        ("idp.example", Kind::Cookie, Some("sso"))
    );
    assert_eq!(c.sites, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(s.overlaps_for("b").count(), 2);
}

#[test]
fn an_item_matching_no_profile_is_dropped_and_counted() {
    let a = d(&["a.example"]);
    let t = [Target {
        site: "a",
        domains: &a,
    }];
    let s = split_state(
        &doc(&[("x", "tracker.example")], &["https://other.example"]),
        &t,
    )
    .unwrap();
    assert_eq!(s.unmatched, 2);
    assert_eq!(part(&s, "a")["cookies"].as_array().unwrap().len(), 0);
}

#[test]
fn no_value_reaches_an_overlap_or_its_debug_print() {
    let (a, b) = (d(&["idp.example"]), d(&["idp.example"]));
    let t = [
        Target {
            site: "a",
            domains: &a,
        },
        Target {
            site: "b",
            domains: &b,
        },
    ];
    let s = split_state(
        &doc(&[("sso", "idp.example")], &["https://idp.example"]),
        &t,
    )
    .unwrap();
    let shown = format!("{:?} {:?}", s.overlaps, s.unmatched);
    assert!(
        !shown.contains(SECRET_COOKIE) && !shown.contains(SECRET_LS),
        "{shown}"
    );
}

/// Property (spec "Testing"): for any state and any profile domains, the
/// split parts together pass `merge_states`. A small deterministic generator
/// stands in for a property-testing crate; the host pool is built so that
/// parent/sub-domain and shared-domain cases come up often.
#[test]
fn split_parts_always_merge_again() {
    const HOSTS: [&str; 9] = [
        "example.com",
        "a.example.com",
        "b.example.com",
        "x.a.example.com",
        "idp.example",
        "other.test",
        "::1",
        "com",
        "",
    ];
    let mut seed: u64 = 0x2360;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for _ in 0..500 {
        let profiles = 2 + next(3);
        let domains: Vec<Vec<String>> = (0..profiles)
            .map(|_| {
                (0..1 + next(2))
                    .map(|_| HOSTS[next(HOSTS.len() - 1)].to_string())
                    .collect()
            })
            .collect();
        let names: Vec<String> = (0..profiles).map(|i| format!("p{i}")).collect();
        let targets: Vec<Target<'_>> = names
            .iter()
            .zip(&domains)
            .map(|(site, domains)| Target { site, domains })
            .collect();
        let cookies: Vec<(String, String)> = (0..next(8))
            .map(|i| {
                let h = HOSTS[next(HOSTS.len())];
                let h = if next(2) == 0 {
                    format!(".{h}")
                } else {
                    h.to_string()
                };
                (format!("c{i}"), h)
            })
            .collect();
        let origins: Vec<String> = (0..next(5))
            .map(|_| match HOSTS[next(HOSTS.len())] {
                "::1" => "http://[::1]:3000".to_string(),
                h => format!("https://{h}:{}", 440 + next(10)),
            })
            .collect();
        let c: Vec<(&str, &str)> = cookies
            .iter()
            .map(|(n, h)| (n.as_str(), h.as_str()))
            .collect();
        let o: Vec<&str> = origins.iter().map(String::as_str).collect();
        let split = split_state(&doc(&c, &o), &targets).unwrap();
        let parts: Vec<Part<'_>> = split
            .parts
            .iter()
            .map(|(site, state)| Part { site, state })
            .collect();
        merge_states(&parts).unwrap_or_else(|e| panic!("domains {domains:?}: {e}"));
    }
}
