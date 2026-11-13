//! Split one session's exported storage state back into its profiles
//! (#2360, spec "Per-profile split export").
//!
//! A merged live session ([`crate::merge::merge_states`]) exports one
//! document holding every profile's cookies and `origins`. Writing it whole
//! to each profile would store every identity under every profile (#2263),
//! so it is split here by host, with the same helpers the merge uses, so the
//! two can never disagree on what "the same host" means.
//!
//! Matching: an item's *host* alone picks its profile. A cookie's `domain`
//! (leading dot stripped, so host-only and domain cookies match alike); an
//! origin's host (scheme and port never matter, bracketed IPv6 kept). An
//! item matches a profile when its host is, or is under, one of the
//! profile's domains.
//!
//! An item matching **more than one** profile goes to **none** of them: the
//! next merged launch would otherwise find both profiles holding that host
//! and refuse to start. The drop is reported per profile ([`Overlap`]) so the
//! caller can tell the user that login is lost; an item matching no profile
//! is counted and dropped.
//!
//! Nothing here logs or returns a cookie value or `localStorage` entry:
//! [`Overlap`] carries host, kind, cookie name and profile names only.

use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::merge::{array, norm_domain, origin_host, str_field, within};

/// One profile to split into: its name and its domains (`allow_domains`, or
/// its login host; normalised by the caller's `profile_domains`).
pub struct Target<'a> {
    pub site: &'a str,
    pub domains: &'a [String],
}

/// Which half of a storage state an item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Cookie,
    Origin,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Cookie => "cookie",
            Kind::Origin => "origin",
        }
    }
}

/// An item dropped because it matched several profiles. Safe to log and to
/// show: no value is carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    pub host: String,
    pub kind: Kind,
    /// The cookie name; `None` for an origin.
    pub name: Option<String>,
    /// Every profile the item matched, in target order.
    pub sites: Vec<String>,
}

/// The split: one storage-state document per target, plus what was dropped.
#[derive(Debug)]
pub struct Split {
    /// Keyed by target site; every target is present, possibly empty.
    pub parts: BTreeMap<String, Vec<u8>>,
    pub overlaps: Vec<Overlap>,
    /// Items matching no target.
    pub unmatched: usize,
}

impl Split {
    /// The overlaps that involve `site`, for its notice.
    pub fn overlaps_for<'s>(&'s self, site: &'s str) -> impl Iterator<Item = &'s Overlap> + 's {
        self.overlaps
            .iter()
            .filter(move |o| o.sites.iter().any(|s| s == site))
    }
}

/// Split `export` among `targets`. Items are written back unchanged
/// (a cookie keeps its leading-dot `domain`, an origin its full string).
pub fn split_state(export: &[u8], targets: &[Target<'_>]) -> Result<Split> {
    if targets.is_empty() {
        bail!("no browser profile to split the session into");
    }
    let value: Value = serde_json::from_slice(export)
        .map_err(|e| anyhow::anyhow!("exported storage state is not valid JSON: {e}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("exported storage state must be a JSON object"))?;

    let mut cookies: Vec<Vec<Value>> = vec![Vec::new(); targets.len()];
    let mut origins: Vec<Vec<Value>> = vec![Vec::new(); targets.len()];
    let mut overlaps = Vec::new();
    let mut unmatched = 0;

    for (kind, items, out) in [
        (Kind::Cookie, array(obj, "cookies", "export")?, &mut cookies),
        (Kind::Origin, array(obj, "origins", "export")?, &mut origins),
    ] {
        for item in items {
            let host = match kind {
                Kind::Cookie => norm_domain(&str_field(item, "domain")),
                Kind::Origin => origin_host(&str_field(item, "origin")),
            };
            let hits = matches(&host, targets);
            match hits.as_slice() {
                [] => unmatched += 1,
                [one] => out[*one].push(item.clone()),
                many => overlaps.push(Overlap {
                    name: (kind == Kind::Cookie).then(|| str_field(item, "name")),
                    host,
                    kind,
                    sites: many.iter().map(|i| targets[*i].site.to_owned()).collect(),
                }),
            }
        }
    }

    let mut parts = BTreeMap::new();
    for (i, t) in targets.iter().enumerate() {
        let doc = json!({
            "cookies": std::mem::take(&mut cookies[i]),
            "origins": std::mem::take(&mut origins[i]),
        });
        parts.insert(t.site.to_owned(), serde_json::to_vec(&doc)?);
    }
    Ok(Split {
        parts,
        overlaps,
        unmatched,
    })
}

/// Indices of the targets whose domains cover `host`. An empty host (a
/// cookie or origin without one) matches nothing.
fn matches(host: &str, targets: &[Target<'_>]) -> Vec<usize> {
    if host.is_empty() {
        return Vec::new();
    }
    targets
        .iter()
        .enumerate()
        .filter(|(_, t)| t.domains.iter().any(|d| within(host, &norm_domain(d))))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
#[path = "split_tests.rs"]
mod tests;
