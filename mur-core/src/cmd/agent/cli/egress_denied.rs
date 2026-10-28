//! Rendering an `egress/denied` event (#1907, spec Part 3).
//!
//! The event is data only (D2); this is the murmur rendering. TUI: one row of
//! chips under a one-line notice. Plain/other clients: the same notice plus a
//! copy-pasteable `/browser allow` command. Deny-list hits get the reason and
//! nothing grantable (D3).

use std::collections::{BTreeSet, HashSet};

use mur_common::egress_event::EgressDenied;

use super::suggest::Suggestion;

/// What the session remembers between denials.
#[derive(Debug, Default)]
pub struct EgressChips {
    /// Hosts the user answered 不用 for: no chip again this session.
    pub dismissed: HashSet<String>,
    /// Hosts granted with `/browser allow --session`: on the running proxy
    /// only, so the profile's allowlist does not show them (#2101).
    pub session_allowed: HashSet<String>,
    /// Every host denied this session, per server — for the wildcard hint.
    pub denied: std::collections::HashMap<String, BTreeSet<String>>,
    /// (server, host) pairs whose notice line is already in the transcript.
    /// A page load hits the same host many times (sub-resources, retries);
    /// one line per pair is enough — the chips still refresh every time.
    noticed: HashSet<(String, String)>,
}

impl EgressChips {
    /// True the first time this session sees `ev`'s (server, host) pair.
    pub fn first_notice(&mut self, ev: &EgressDenied) -> bool {
        self.noticed.insert((ev.server.clone(), ev.host.clone()))
    }

    /// The transcript was cleared (`/new`, channel switch): its notice lines
    /// are gone, so every host is noticed afresh. `dismissed`/`denied` are
    /// the user's answers, not screen state, and stay.
    pub fn forget_notices(&mut self) {
        self.noticed.clear();
    }
}

/// The registrable-ish parent of `host`: everything after the first label.
/// `img.shop.example.com` → `shop.example.com`. `None` for a two-label host
/// or an IP, where a wildcard would cover a whole public suffix.
fn parent(host: &str) -> Option<&str> {
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return None;
    }
    let (_, rest) = host.split_once('.')?;
    rest.contains('.').then_some(rest)
}

/// `*.parent` when ≥2 denied hosts share a parent that is (or is under) a
/// domain already on the allow-list (spec Part 3 §4).
pub fn wildcard_suggestion(denied: &BTreeSet<String>, allow: &[String]) -> Option<String> {
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    for h in denied {
        if let Some(p) = parent(h) {
            *counts.entry(p).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(p, _)| p)
        .find(|p| {
            allow.iter().any(|a| {
                let a = a.trim_start_matches("*.").trim_start_matches('.');
                *p == a || p.ends_with(&format!(".{a}"))
            })
        })
        .map(|p| format!("*.{p}"))
}

/// The one-line notice every client shows.
pub fn notice(ev: &EgressDenied) -> String {
    if ev.grantable() {
        format!("`{}` was blocked for `{}`.", ev.host, ev.server)
    } else {
        format!(
            "`{}` is on `{}`'s deny list — it stays blocked.",
            ev.host, ev.server
        )
    }
}

/// Text fallback for clients without chips (plain mode, Commander/Slack).
pub fn text_fallback(ev: &EgressDenied) -> String {
    if ev.grantable() && ev.server == super::browser_live_cmd::LIVE_ENTRY {
        format!(
            "{} To always allow it: `/browser allow {}` (this run only: `/browser allow --session {}`)",
            notice(ev),
            ev.host,
            ev.host
        )
    } else {
        notice(ev)
    }
}

/// The chooser rows for one denial, or `None` when no chip should show:
/// deny-listed (D3), dismissed, or a server `/browser allow` cannot target.
/// The 不用 row is a slash no-op the TUI intercepts to record the dismissal.
pub fn chips(
    ev: &EgressDenied,
    state: &mut EgressChips,
    allow: &[String],
) -> Option<Vec<Suggestion>> {
    let seen = state.denied.entry(ev.server.clone()).or_default();
    seen.insert(ev.host.clone());
    if !ev.grantable()
        || ev.server != super::browser_live_cmd::LIVE_ENTRY
        || state.dismissed.contains(&ev.host)
    {
        return None;
    }
    let permanent = wildcard_suggestion(seen, allow).unwrap_or_else(|| ev.host.clone());
    let item = |text: String, desc: &str| Suggestion {
        text,
        desc: Some(desc.to_owned()),
    };
    Some(vec![
        item(
            format!("/browser allow --session {}", ev.host),
            "這次放行 — this run only, nothing saved",
        ),
        item(
            format!("/browser allow {permanent}"),
            "永久放行 — preview, then save to the profile",
        ),
        item(
            format!("{DISMISS_PREFIX}{}", ev.host),
            "不用 — don't ask again this session",
        ),
    ])
}

/// Chooser text for 不用. Intercepted before it reaches the slash parser.
pub const DISMISS_PREFIX: &str = "/browser allow --dismiss ";

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::egress_event::DenyReason;

    fn ev(server: &str, host: &str, reason: DenyReason) -> EgressDenied {
        EgressDenied {
            agent: "mur".into(),
            server: server.into(),
            host: host.into(),
            port: Some(443),
            mode: "restricted".into(),
            reason,
            at: "2026-10-15T00:00:00Z".into(),
            task_id: None,
        }
    }

    #[test]
    fn grantable_denial_offers_three_chips() {
        let mut st = EgressChips::default();
        let c = chips(
            &ev("browser", "img.shop.example.com", DenyReason::NotAllowed),
            &mut st,
            &[],
        )
        .unwrap();
        let t: Vec<&str> = c.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            t,
            vec![
                "/browser allow --session img.shop.example.com",
                "/browser allow img.shop.example.com",
                "/browser allow --dismiss img.shop.example.com",
            ]
        );
    }

    #[test]
    fn repeated_denials_of_one_host_notice_once() {
        let mut st = EgressChips::default();
        let e = ev("browser", "gator.volces.com", DenyReason::NotAllowed);
        assert!(st.first_notice(&e));
        assert!(!st.first_notice(&e));
        assert!(!st.first_notice(&e));
        // Another host, or the same host on another server, still notices.
        assert!(st.first_notice(&ev("browser", "x.volces.com", DenyReason::NotAllowed)));
        assert!(st.first_notice(&ev("fetch", "gator.volces.com", DenyReason::NotAllowed)));
    }

    #[test]
    fn deny_list_hit_shows_reason_only() {
        let mut st = EgressChips::default();
        let e = ev("browser", "169.254.169.254", DenyReason::DenyListed);
        assert!(chips(&e, &mut st, &[]).is_none());
        let t = text_fallback(&e);
        assert!(
            t.contains("deny list") && !t.contains("/browser allow"),
            "{t}"
        );
    }

    #[test]
    fn dismissed_hosts_and_other_servers_get_no_chip() {
        let mut st = EgressChips::default();
        st.dismissed.insert("a.example.com".into());
        assert!(
            chips(
                &ev("browser", "a.example.com", DenyReason::NotAllowed),
                &mut st,
                &[]
            )
            .is_none()
        );
        assert!(
            chips(
                &ev("fetch", "b.example.com", DenyReason::NotAllowed),
                &mut st,
                &[]
            )
            .is_none()
        );
        assert_eq!(
            text_fallback(&ev("fetch", "b.example.com", DenyReason::NotAllowed)),
            "`b.example.com` was blocked for `fetch`."
        );
    }

    #[test]
    fn second_denial_under_an_allowed_domain_suggests_a_wildcard() {
        let mut st = EgressChips::default();
        let allow = vec!["shop.example.com".to_owned()];
        let first = chips(
            &ev("browser", "img.shop.example.com", DenyReason::NotAllowed),
            &mut st,
            &allow,
        )
        .unwrap();
        assert_eq!(first[1].text, "/browser allow img.shop.example.com");
        let second = chips(
            &ev("browser", "cdn.shop.example.com", DenyReason::NotAllowed),
            &mut st,
            &allow,
        )
        .unwrap();
        assert_eq!(second[1].text, "/browser allow *.shop.example.com");
        // The this-time chip still names the one host.
        assert_eq!(
            second[0].text,
            "/browser allow --session cdn.shop.example.com"
        );
    }

    #[test]
    fn no_wildcard_over_a_bare_suffix_or_an_unlisted_domain() {
        // Two-label hosts: parent() refuses to widen to `*.com`.
        let denied: BTreeSet<String> = ["a.com", "b.com"].map(String::from).into();
        assert_eq!(wildcard_suggestion(&denied, &["com".into()]), None);
        let denied: BTreeSet<String> = ["x.cdn.net", "y.cdn.net"].map(String::from).into();
        assert_eq!(
            wildcard_suggestion(&denied, &["shop.example.com".into()]),
            None
        );
        let ips: BTreeSet<String> = ["10.0.0.1", "10.0.0.2"].map(String::from).into();
        assert_eq!(wildcard_suggestion(&ips, &["10.0.0.1".into()]), None);
    }
}
