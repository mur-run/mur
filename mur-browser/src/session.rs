//! Which parts of a Playwright storage state are the login session.
//!
//! A storage state holds every cookie the site set, and most of them are not
//! the session: analytics IDs and anti-bot tokens (Cloudflare's `__cf_bm`
//! lives 30 minutes and rotates) used to dominate `earliest_cookie_expires`,
//! so `mur browser status` showed a profile "expiring" half an hour after a
//! login that was still good for weeks. This module keeps only the cookies
//! that can plausibly carry the login, and answers whether a state still has
//! any session material at all (the write-back guard in [`crate::refresh`]).

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

/// Cookie names that never carry a login: analytics and bot-mitigation
/// tokens, matched exactly.
const NON_SESSION_COOKIES: &[&str] = &[
    "__cf_bm",
    "cf_clearance",
    "__cfruid",
    "_cfuvid",
    "_fbp",
    "_fbc",
];

/// Cookie name prefixes of analytics families (`_ga`, `_ga_<id>`, `_gat_*`,
/// `__utma`, Hotjar `_hj*`, Google Ads `_gcl_*`, `_gid`).
const NON_SESSION_PREFIXES: &[&str] = &["_ga", "_gid", "__utm", "_hj", "_gcl"];

/// A cookie that dies sooner than this after capture is a rotating token
/// (WAF / rate-limit), not a login: a site that really ends sessions this fast
/// renews them while you use it, which the write-back now records.
pub const MIN_SESSION_COOKIE_LIFETIME: Duration = Duration::hours(1);

/// Earliest expiry among the cookies that may carry the login, judged at
/// `captured_at`. `None` when no such cookie has an expiry — the session is a
/// browser-session cookie or lives in web storage, so nothing on disk says
/// when it ends.
pub fn session_cookie_expiry(
    storage_state: &[u8],
    allow_domains: &[String],
    captured_at: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    let value: Value = serde_json::from_slice(storage_state)?;
    let cookies = value
        .get("cookies")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("browser storage state must contain an array field \"cookies\"")
        })?;
    let floor = captured_at + MIN_SESSION_COOKIE_LIFETIME;
    Ok(cookies
        .iter()
        .filter(|cookie| is_session_candidate(cookie, allow_domains))
        .filter_map(|cookie| cookie.get("expires").and_then(Value::as_f64))
        .filter(|expires| *expires > 0.0 && expires.is_finite())
        .filter_map(|expires| DateTime::from_timestamp(expires as i64, 0))
        .filter(|expires| *expires >= floor)
        .min())
}

/// True when the state still holds something a login could live in for the
/// profile's domains: a session-candidate cookie, or non-empty localStorage
/// on an allowed origin. A state with neither is a logged-out or fresh
/// context and must never replace a saved profile.
pub fn has_session_data(storage_state: &[u8], allow_domains: &[String]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(storage_state) else {
        return false;
    };
    let cookie = value
        .get("cookies")
        .and_then(Value::as_array)
        .is_some_and(|cookies| {
            cookies
                .iter()
                .any(|cookie| is_session_candidate(cookie, allow_domains))
        });
    let storage = value
        .get("origins")
        .and_then(Value::as_array)
        .is_some_and(|origins| {
            origins.iter().any(|origin| {
                let allowed = origin
                    .get("origin")
                    .and_then(Value::as_str)
                    .is_some_and(|url| crate::guard::is_allowed(url, allow_domains));
                let filled = origin
                    .get("localStorage")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty());
                allowed && filled
            })
        });
    cookie || storage
}

fn is_session_candidate(cookie: &Value, allow_domains: &[String]) -> bool {
    let name = cookie.get("name").and_then(Value::as_str).unwrap_or("");
    if NON_SESSION_COOKIES.contains(&name)
        || NON_SESSION_PREFIXES.iter().any(|p| name.starts_with(p))
    {
        return false;
    }
    if allow_domains.is_empty() {
        return true;
    }
    let domain = cookie.get("domain").and_then(Value::as_str).unwrap_or("");
    let domain = domain.trim_start_matches('.');
    // Either direction: `.example.com` is sent to `app.example.com`, and an
    // `example.com` entry covers a `chat.example.com` cookie.
    allow_domains.iter().any(|entry| {
        crate::guard::host_matches(domain, entry)
            || crate::guard::host_matches(entry.trim_start_matches('.'), domain)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap()
    }

    fn allow(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    const NOW: i64 = 1_700_000_000;

    #[test]
    fn rotating_anti_bot_and_analytics_cookies_do_not_set_the_expiry() {
        let state = format!(
            r#"{{"cookies":[
                {{"name":"__cf_bm","domain":".example.com","expires":{cf}}},
                {{"name":"_ga_X1","domain":".example.com","expires":{ga}}},
                {{"name":"session","domain":"example.com","expires":{sess}}}
            ],"origins":[]}}"#,
            cf = NOW + 1_800,
            ga = NOW + 3_600 * 2,
            sess = NOW + 86_400 * 30,
        );
        let got = session_cookie_expiry(state.as_bytes(), &allow(&["example.com"]), at(NOW));
        assert_eq!(got.unwrap(), Some(at(NOW + 86_400 * 30)));
    }

    #[test]
    fn short_lived_unknown_cookies_are_treated_as_rotating_tokens() {
        let state = format!(
            r#"{{"cookies":[{{"name":"HWWAFSESTIME","domain":"chat.example.com","expires":{}}}],"origins":[]}}"#,
            NOW + 600
        );
        let got = session_cookie_expiry(state.as_bytes(), &[], at(NOW)).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn cookies_outside_the_allowlist_are_ignored() {
        let state = format!(
            r#"{{"cookies":[{{"name":"sid","domain":".tracker.test","expires":{}}}],"origins":[]}}"#,
            NOW + 86_400
        );
        let got = session_cookie_expiry(state.as_bytes(), &allow(&["example.com"]), at(NOW));
        assert_eq!(got.unwrap(), None);
    }

    #[test]
    fn parent_domain_cookie_counts_for_a_subdomain_entry() {
        let state = format!(
            r#"{{"cookies":[{{"name":"sid","domain":".example.com","expires":{}}}],"origins":[]}}"#,
            NOW + 86_400
        );
        let got = session_cookie_expiry(state.as_bytes(), &allow(&["chat.example.com"]), at(NOW));
        assert_eq!(got.unwrap(), Some(at(NOW + 86_400)));
    }

    #[test]
    fn local_storage_on_an_allowed_origin_is_session_data() {
        let state = br#"{"cookies":[{"name":"__cf_bm","domain":".example.com"}],
            "origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"userToken","value":"x"}]}]}"#;
        assert!(has_session_data(state, &allow(&["example.com"])));
    }

    #[test]
    fn empty_or_logged_out_state_has_no_session_data() {
        assert!(!has_session_data(br#"{"cookies":[],"origins":[]}"#, &[]));
        let only_bot = br#"{"cookies":[{"name":"__cf_bm","domain":".example.com"}],
            "origins":[{"origin":"https://evil.test","localStorage":[{"name":"t","value":"x"}]}]}"#;
        assert!(!has_session_data(only_bot, &allow(&["example.com"])));
        assert!(!has_session_data(b"not json", &[]));
    }
}
