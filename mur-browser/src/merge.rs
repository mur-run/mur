//! Merge several saved storage states into one launch document (#2263).
//!
//! An agent's live browser may carry more than one saved profile. Playwright
//! takes exactly one `--storage-state` file, so the decrypted states are
//! joined here: cookies are concatenated and `origins` entries for the same
//! origin are folded into one `localStorage` list.
//!
//! Two profiles for the same host would collide — the browser keeps whichever
//! cookie it sees last, silently picking one identity. The binder refuses
//! that at bind time ([`BindPlan::Overlaps`]) from the profiles' declared
//! domains; this merge re-checks on the actual cookies so a profile whose
//! session grew a shared domain after binding still fails loudly instead of
//! launching half logged-in.
//!
//! [`BindPlan::Overlaps`]: ../../mur_core/cmd/browser/bind_plan/enum.BindPlan.html

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
    // Cookie identity as the browser sees it: (name, domain, path). The same
    // tuple from two profiles is the collision the bind-time check exists to
    // prevent; it is refused here too, naming both profiles.
    let mut seen: BTreeMap<(String, String, String), &str> = BTreeMap::new();
    let mut origins: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut origin_order: Vec<String> = Vec::new();
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
        for cookie in array(obj, "cookies", part.site)? {
            let key = (
                str_field(cookie, "name"),
                str_field(cookie, "domain")
                    .trim_start_matches('.')
                    .to_ascii_lowercase(),
                str_field(cookie, "path"),
            );
            if let Some(other) = seen.insert(key.clone(), part.site)
                && other != part.site
            {
                bail!(
                    "browser profiles {other:?} and {:?} both set cookie {:?} for {}{} — one \
                     live session cannot hold two logins for the same host; unbind one \
                     (`/browser live <hosts> --no-profile <site>`)",
                    part.site,
                    key.0,
                    key.1,
                    key.2
                );
            }
            cookies.push(cookie.clone());
        }
        for origin in array(obj, "origins", part.site)? {
            let name = str_field(origin, "origin");
            let items = origin
                .get("localStorage")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            match origins.get_mut(&name) {
                Some(existing) => {
                    let have: BTreeSet<String> =
                        existing.iter().map(|i| str_field(i, "name")).collect();
                    existing.extend(
                        items
                            .into_iter()
                            .filter(|i| !have.contains(&str_field(i, "name"))),
                    );
                }
                None => {
                    origin_order.push(name.clone());
                    origins.insert(name, items);
                }
            }
        }
    }
    let origins: Vec<Value> = origin_order
        .into_iter()
        .map(|name| {
            let items = origins.remove(&name).unwrap_or_default();
            json!({ "origin": name, "localStorage": items })
        })
        .collect();
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
    fn disjoint_profiles_keep_every_cookie_and_fold_origins() {
        let a = state(
            &[("sid", ".a.example")],
            &[("https://a.example", &[("token", "1")])],
        );
        let b = state(
            &[("sess", "b.example")],
            &[
                ("https://a.example", &[("theme", "dark")]),
                ("https://b.example", &[("jwt", "2")]),
            ],
        );
        let v = vec![("a", a), ("b", b)];
        let out: Value = serde_json::from_slice(&merge_states(&parts(&v)).unwrap()).unwrap();
        let cookies = out["cookies"].as_array().unwrap();
        assert_eq!(cookies.len(), 2);
        let origins = out["origins"].as_array().unwrap();
        assert_eq!(origins.len(), 2, "{origins:?}");
        assert_eq!(origins[0]["origin"], "https://a.example");
        assert_eq!(origins[0]["localStorage"].as_array().unwrap().len(), 2);
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
        assert!(err.contains("--no-profile"), "{err}");
    }

    #[test]
    fn the_first_origin_wins_a_duplicate_local_storage_key() {
        let a = state(&[], &[("https://x.example", &[("k", "first")])]);
        let b = state(&[], &[("https://x.example", &[("k", "second")])]);
        let v = vec![("a", a), ("b", b)];
        let out: Value = serde_json::from_slice(&merge_states(&parts(&v)).unwrap()).unwrap();
        let items = out["origins"][0]["localStorage"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["value"], "first");
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
