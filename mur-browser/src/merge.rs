//! Merge several saved storage states into one launch document (#2263).
//!
//! An agent's live browser may carry more than one saved profile. Playwright
//! takes exactly one `--storage-state` file, so the decrypted states are
//! joined here: cookies and `origins` entries are concatenated.
//!
//! Two profiles for the same host would collide — the browser keeps whichever
//! cookie it sees last, silently picking one identity. The binder refuses
//! that at bind time ([`BindPlan::Overlaps`]) from the profiles' declared
//! domains; this merge re-checks the same rule on the *saved* state — every
//! cookie domain and every `origins` host, parent and subdomain alike — so a
//! session that grew a shared domain after binding (a login through one
//! identity provider, say) still fails loudly instead of launching half
//! logged-in. Cookie names and paths are irrelevant to that: a `.idp.example`
//! cookie from either profile reaches the same server.
//!
//! [`BindPlan::Overlaps`]: ../../mur_core/cmd/browser/bind_plan/enum.BindPlan.html

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

/// One decrypted storage state, labelled by the profile it came from.
pub struct Part<'a> {
    pub site: &'a str,
    pub state: &'a [u8],
}

/// A single Playwright storage-state document holding every part's session.
/// A single part is returned as it was saved, byte for byte.
pub fn merge_states(parts: &[Part<'_>]) -> Result<Vec<u8>> {
    match parts {
        [] => bail!("no browser profile state to merge"),
        [one] => return Ok(one.state.to_vec()),
        _ => {}
    }
    let mut cookies: Vec<Value> = Vec::new();
    // Every host a part's state would be sent to, keyed by profile. A host
    // of one part that is, or is under, a host of an earlier part is the
    // collision the bind-time check exists to prevent; it is refused here
    // too, naming both profiles.
    let mut claimed: Vec<(&str, BTreeSet<String>)> = Vec::new();
    let mut origins: Vec<Value> = Vec::new();
    for part in parts {
        let value: Value = serde_json::from_slice(part.state).map_err(|e| {
            anyhow::anyhow!(
                "browser profile {:?}: storage state is not valid JSON: {e}",
                part.site
            )
        })?;
        let obj = value.as_object().ok_or_else(|| {
            anyhow::anyhow!(
                "browser profile {:?}: storage state must be a JSON object",
                part.site
            )
        })?;
        let mut mine: BTreeSet<String> = BTreeSet::new();
        for cookie in array(obj, "cookies", part.site)? {
            mine.insert(norm_domain(&str_field(cookie, "domain")));
            cookies.push(cookie.clone());
        }
        for origin in array(obj, "origins", part.site)? {
            mine.insert(origin_host(&str_field(origin, "origin")));
            origins.push(origin.clone());
        }
        mine.remove("");
        for (other, theirs) in &claimed {
            if let Some(shared) = mine
                .iter()
                .find(|m| theirs.iter().any(|t| within(m, t) || within(t, m)))
            {
                bail!(
                    "browser profiles {other:?} and {:?} both hold a session for {shared} — one \
                     live session cannot hold two logins for the same host; rerun \
                     `/browser live <hosts> --profile <site>` naming only the profiles to keep",
                    part.site,
                );
            }
        }
        claimed.push((part.site, mine));
    }
    Ok(serde_json::to_vec(&json!({
        "cookies": cookies,
        "origins": origins,
    }))?)
}

fn array<'v>(obj: &'v Map<String, Value>, field: &str, site: &str) -> Result<&'v Vec<Value>> {
    obj.get(field).and_then(Value::as_array).ok_or_else(|| {
        anyhow::anyhow!(
            "browser profile {site:?}: storage state must contain an array field {field:?}"
        )
    })
}

/// Cookie domain as the browser matches it: no leading dot, lower-case.
fn norm_domain(domain: &str) -> String {
    domain.trim_start_matches('.').to_ascii_lowercase()
}

/// Host of a storage-state `origin` (`https://chat.x.example:8443` →
/// `chat.x.example`). An origin that is not a URL is kept as written.
fn origin_host(origin: &str) -> String {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    norm_domain(host.split(':').next().unwrap_or(host))
}

/// `a` is `b` or a subdomain of it — the same rule the binder applies to
/// declared domains (`mur_core::cmd::browser::live_entry::within`).
fn within(a: &str, b: &str) -> bool {
    a == b || a.strip_suffix(b).is_some_and(|p| p.ends_with('.'))
}

fn str_field(v: &Value, field: &str) -> String {
    v.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(cookies: &[(&str, &str)], origins: &[(&str, &[(&str, &str)])]) -> Vec<u8> {
        let cookies: Vec<Value> = cookies
            .iter()
            .map(|(name, domain)| {
                json!({ "name": name, "value": "v", "domain": domain, "path": "/" })
            })
            .collect();
        let origins: Vec<Value> = origins
            .iter()
            .map(|(origin, items)| {
                let items: Vec<Value> = items
                    .iter()
                    .map(|(n, v)| json!({ "name": n, "value": v }))
                    .collect();
                json!({ "origin": origin, "localStorage": items })
            })
            .collect();
        serde_json::to_vec(&json!({ "cookies": cookies, "origins": origins })).unwrap()
    }

    fn parts<'a>(v: &'a [(&'a str, Vec<u8>)]) -> Vec<Part<'a>> {
        v.iter().map(|(site, state)| Part { site, state }).collect()
    }

    #[test]
    fn a_single_part_is_returned_verbatim() {
        let a = state(&[("sid", ".a.example")], &[]);
        let v = vec![("a", a.clone())];
        assert_eq!(merge_states(&parts(&v)).unwrap(), a);
    }

    #[test]
    fn disjoint_profiles_keep_every_cookie_and_origin() {
        let a = state(
            &[("sid", ".a.example")],
            &[("https://a.example", &[("token", "1")])],
        );
        let b = state(
            &[("sess", "b.example"), ("pref", "cdn.b.example")],
            &[("https://b.example", &[("jwt", "2")])],
        );
        let v = vec![("a", a), ("b", b)];
        let out: Value = serde_json::from_slice(&merge_states(&parts(&v)).unwrap()).unwrap();
        let cookies = out["cookies"].as_array().unwrap();
        assert_eq!(cookies.len(), 3);
        let origins = out["origins"].as_array().unwrap();
        assert_eq!(origins.len(), 2, "{origins:?}");
        assert_eq!(origins[0]["origin"], "https://a.example");
        assert_eq!(origins[1]["origin"], "https://b.example");
    }

    #[test]
    fn the_same_cookie_from_two_profiles_is_refused_naming_both() {
        let a = state(&[("sid", ".shared.example")], &[]);
        let b = state(&[("sid", "shared.example")], &[]);
        let v = vec![("one", a), ("two", b)];
        let err = merge_states(&parts(&v)).unwrap_err().to_string();
        assert!(err.contains("\"one\"") && err.contains("\"two\""), "{err}");
        assert!(err.contains("shared.example"), "{err}");
        assert!(err.contains("--profile <site>"), "{err}");
    }

    #[test]
    fn different_cookies_on_a_shared_domain_are_refused() {
        // Two sites that both log in through one identity provider: the
        // cookie names differ, so a duplicate-identity check passes, but the
        // browser would send both to `idp.example` and pick one login.
        let a = state(&[("sid", "a.example"), ("idp_a", ".idp.example")], &[]);
        let b = state(
            &[("sess", "b.example"), ("idp_b", "login.idp.example")],
            &[],
        );
        let v = vec![("a", a), ("b", b)];
        let err = merge_states(&parts(&v)).unwrap_err().to_string();
        assert!(err.contains("\"a\"") && err.contains("\"b\""), "{err}");
        assert!(err.contains("idp.example"), "{err}");
    }

    #[test]
    fn a_cookie_on_a_parent_domain_of_the_other_profile_is_refused() {
        let a = state(&[("sid", ".corp.example")], &[]);
        let b = state(&[("jwt", "chat.corp.example")], &[]);
        let v = vec![("one", a), ("two", b)];
        let err = merge_states(&parts(&v)).unwrap_err().to_string();
        assert!(err.contains("corp.example"), "{err}");
    }

    #[test]
    fn a_shared_origin_local_storage_is_refused_not_folded() {
        // Disjoint cookies, but both profiles hold localStorage for one
        // origin: that is session state too (JWTs live there), and keeping
        // only the first silently drops the other login.
        let a = state(
            &[("sid", "a.example")],
            &[("https://x.example", &[("k", "first")])],
        );
        let b = state(
            &[("sess", "b.example")],
            &[("https://x.example", &[("k", "second")])],
        );
        let v = vec![("a", a), ("b", b)];
        let err = merge_states(&parts(&v)).unwrap_err().to_string();
        assert!(err.contains("x.example"), "{err}");
    }

    #[test]
    fn origin_host_strips_scheme_port_and_dot() {
        assert_eq!(origin_host("https://Chat.X.example:8443"), "chat.x.example");
        assert_eq!(origin_host("http://x.example/"), "x.example");
        assert_eq!(norm_domain(".X.example"), "x.example");
    }

    #[test]
    fn a_malformed_part_names_its_profile() {
        let v = vec![("good", state(&[], &[])), ("bad", b"{}".to_vec())];
        let err = merge_states(&parts(&v)).unwrap_err().to_string();
        assert!(err.contains("\"bad\"") && err.contains("cookies"), "{err}");
    }

    #[test]
    fn nothing_to_merge_is_an_error() {
        assert!(merge_states(&[]).is_err());
    }
}
