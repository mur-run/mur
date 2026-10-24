//! Which parts of a Playwright storage state are the login session.
//!
//! A storage state holds every cookie the site set, and most of them are not
//! the session: analytics IDs and anti-bot tokens (Cloudflare's `__cf_bm`
//! lives 30 minutes and rotates) used to dominate `earliest_cookie_expires`,
//! so `mur browser status` showed a profile "expiring" half an hour after a
//! login that was still good for weeks. This module keeps only the cookies
//! that can plausibly carry the login, and answers whether an exported state
//! still carries the saved login (the write-back guard in [`crate::refresh`]).

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::collections::BTreeSet;

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

/// One place a login may live: a session-candidate cookie or a localStorage
/// key on an allowed origin, identified by name and holding a non-cleared
/// value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoginKey {
    Cookie(String),
    Storage(String),
}

/// The login keys of a state captured at `captured_at`, still meaningful at
/// `now`: session-candidate cookies (minus rotating short-lived ones and ones
/// already expired) and localStorage keys on allowed origins.
pub fn login_keys(
    storage_state: &[u8],
    allow_domains: &[String],
    captured_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<BTreeSet<LoginKey>> {
    let value = serde_json::from_slice::<Value>(storage_state).ok()?;
    let floor = captured_at + MIN_SESSION_COOKIE_LIFETIME;
    let mut keys = BTreeSet::new();
    for cookie in value.get("cookies").and_then(Value::as_array)? {
        if !is_session_candidate(cookie, allow_domains) {
            continue;
        }
        let expires = cookie
            .get("expires")
            .and_then(Value::as_f64)
            .unwrap_or(-1.0);
        // `-1` is a browser-session cookie: no expiry, often the login itself.
        if expires > 0.0 && expires.is_finite() {
            let Some(at) = DateTime::from_timestamp(expires as i64, 0) else {
                continue;
            };
            if at < floor || at <= now {
                continue;
            }
        }
        if let Some(name) = cookie.get("name").and_then(Value::as_str)
            && holds_value(cookie)
        {
            keys.insert(LoginKey::Cookie(name.to_owned()));
        }
    }
    for origin in value
        .get("origins")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let allowed = origin
            .get("origin")
            .and_then(Value::as_str)
            .is_some_and(|url| crate::guard::is_allowed(url, allow_domains));
        if !allowed {
            continue;
        }
        let items = origin.get("localStorage").and_then(Value::as_array);
        for item in items.into_iter().flatten() {
            if let Some(name) = item.get("name").and_then(Value::as_str)
                && holds_value(item)
            {
                keys.insert(LoginKey::Storage(name.to_owned()));
            }
        }
    }
    Some(keys)
}

/// Positive login check for write-back. Site storage alone proves nothing (a
/// logged-out page keeps `theme=dark`, consent and device-ID cookies), so the
/// reference is the saved profile, captured right after a confirmed login:
/// the export must still hold **every** login key the saved state has, each
/// with a non-cleared value. A logout removes or blanks the token, so a key
/// goes missing and the profile is kept.
/// When the saved state has no identifiable keys, login cannot be
/// established and the answer is `false` as well.
pub fn retains_login(
    saved: &[u8],
    saved_at: DateTime<Utc>,
    export: &[u8],
    allow_domains: &[String],
    now: DateTime<Utc>,
) -> bool {
    let Some(required) = login_keys(saved, allow_domains, saved_at, now) else {
        return false;
    };
    // The export is judged as captured now; its cookies only need to exist.
    let Some(present) = login_keys(
        export,
        allow_domains,
        now - MIN_SESSION_COOKIE_LIFETIME,
        now,
    ) else {
        return false;
    };
    !required.is_empty() && required.is_subset(&present)
}

/// Values a site writes when it clears a credential in place instead of
/// removing the key (`setItem("userToken", "")`, `String(null)`, ...).
const CLEARED_VALUES: &[&str] = &["", "null", "undefined"];

/// Whether an entry still carries something: a key whose value was cleared
/// is not login material, so a logout that blanks the token without removing
/// it drops the key from the set and fails [`retains_login`].
fn holds_value(entry: &Value) -> bool {
    entry
        .get("value")
        .and_then(Value::as_str)
        .is_some_and(|v| !CLEARED_VALUES.contains(&v.trim()))
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
            r#"{{"cookies":[{{"name":"HWWAFSESTIME","value":"v","domain":"chat.example.com","expires":{}}}],"origins":[]}}"#,
            NOW + 600
        );
        let got = session_cookie_expiry(state.as_bytes(), &[], at(NOW)).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn cookies_outside_the_allowlist_are_ignored() {
        let state = format!(
            r#"{{"cookies":[{{"name":"sid","value":"v","domain":".tracker.test","expires":{}}}],"origins":[]}}"#,
            NOW + 86_400
        );
        let got = session_cookie_expiry(state.as_bytes(), &allow(&["example.com"]), at(NOW));
        assert_eq!(got.unwrap(), None);
    }

    #[test]
    fn parent_domain_cookie_counts_for_a_subdomain_entry() {
        let state = format!(
            r#"{{"cookies":[{{"name":"sid","value":"v","domain":".example.com","expires":{}}}],"origins":[]}}"#,
            NOW + 86_400
        );
        let got = session_cookie_expiry(state.as_bytes(), &allow(&["chat.example.com"]), at(NOW));
        assert_eq!(got.unwrap(), Some(at(NOW + 86_400)));
    }

    const SAVED: &[u8] = br#"{"cookies":[
            {"name":"__cf_bm","domain":".example.com","expires":1700001800},
            {"name":"consent","value":"v","domain":".example.com","expires":1800000000}],
        "origins":[{"origin":"https://chat.example.com","localStorage":[
            {"name":"userToken","value":"old"},{"name":"theme","value":"dark"}]}]}"#;

    #[test]
    fn rotated_login_is_retained() {
        let export =
            br#"{"cookies":[{"name":"consent","value":"v","domain":".example.com","expires":1800000000}],
            "origins":[{"origin":"https://chat.example.com","localStorage":[
                {"name":"userToken","value":"new"},{"name":"theme","value":"dark"}]}]}"#;
        assert!(retains_login(
            SAVED,
            at(NOW),
            export,
            &allow(&["example.com"]),
            at(NOW + 60)
        ));
    }

    #[test]
    fn logout_leaving_preferences_and_consent_is_not_a_login() {
        let export = br#"{"cookies":[{"name":"consent","value":"v","domain":".example.com","expires":1800000000}],
            "origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"theme","value":"dark"}]}]}"#;
        assert!(!retains_login(
            SAVED,
            at(NOW),
            export,
            &allow(&["example.com"]),
            at(NOW + 60)
        ));
        let only_theme = br#"{"cookies":[],
            "origins":[{"origin":"https://chat.example.com","localStorage":[{"name":"theme","value":"dark"}]}]}"#;
        assert!(!retains_login(
            SAVED,
            at(NOW),
            only_theme,
            &allow(&["example.com"]),
            at(NOW + 60)
        ));
    }

    #[test]
    fn logout_that_clears_but_keeps_the_token_key_is_not_a_login() {
        let allowed = allow(&["example.com"]);
        for cleared in [r#""""#, r#""  ""#, "null", r#""null""#, r#""undefined""#] {
            let export = format!(
                r#"{{"cookies":[],"origins":[{{"origin":"https://chat.example.com","localStorage":[
                    {{"name":"userToken","value":{cleared}}},{{"name":"theme","value":"dark"}}]}}]}}"#
            );
            assert!(
                !retains_login(SAVED, at(NOW), export.as_bytes(), &allowed, at(NOW + 60)),
                "cleared value {cleared} must not count as a login"
            );
        }
        let cleared_cookie =
            br#"{"cookies":[{"name":"sid","value":"","domain":".example.com","expires":-1}],
            "origins":[]}"#;
        let saved_cookie =
            br#"{"cookies":[{"name":"sid","value":"abc","domain":".example.com","expires":-1}],
            "origins":[]}"#;
        assert!(!retains_login(
            saved_cookie,
            at(NOW),
            cleared_cookie,
            &allowed,
            at(NOW + 60)
        ));
    }

    #[test]
    fn blank_garbage_or_keyless_states_never_establish_login() {
        let allowed = allow(&["example.com"]);
        let blank = br#"{"cookies":[],"origins":[]}"#;
        assert!(!retains_login(SAVED, at(NOW), blank, &allowed, at(NOW)));
        assert!(!retains_login(
            SAVED,
            at(NOW),
            b"not json",
            &allowed,
            at(NOW)
        ));
        // Nothing identifiable in the saved state: refuse rather than guess.
        assert!(!retains_login(blank, at(NOW), SAVED, &allowed, at(NOW)));
    }

    #[test]
    fn rotating_or_expired_saved_cookies_are_not_required() {
        // __cf_bm (30 min) is not required; consent expired before `now`.
        let saved = br#"{"cookies":[
                {"name":"HWWAFSESTIME","value":"v","domain":"chat.example.com","expires":1700000600},
                {"name":"old","value":"v","domain":".example.com","expires":1700090000},
                {"name":"sid","value":"v","domain":".example.com","expires":-1}],
            "origins":[]}"#;
        let export =
            br#"{"cookies":[{"name":"sid","value":"v","domain":".example.com","expires":-1}],"origins":[]}"#;
        assert!(retains_login(
            saved,
            at(NOW),
            export,
            &allow(&["example.com"]),
            at(1_700_100_000)
        ));
        let gone = br#"{"cookies":[],"origins":[]}"#;
        assert!(!retains_login(
            saved,
            at(NOW),
            gone,
            &allow(&["example.com"]),
            at(1_700_100_000)
        ));
    }

    #[test]
    fn other_origins_do_not_count() {
        let export =
            br#"{"cookies":[{"name":"consent","value":"v","domain":".example.com","expires":1800000000}],
            "origins":[{"origin":"https://evil.test","localStorage":[
                {"name":"userToken","value":"x"},{"name":"theme","value":"dark"}]}]}"#;
        assert!(!retains_login(
            SAVED,
            at(NOW),
            export,
            &allow(&["example.com"]),
            at(NOW + 60)
        ));
    }
}
