//! Whether a saved `mur browser auth` profile may be attached to an agent's
//! live `browser` entry (#2199). Pure: no filesystem, no writes. Both
//! `mur browser status` (warnings) and the binder (PR 3) decide from here, so
//! what status flags and what the binder refuses cannot drift apart.
//!
//! Binding only rewrites `--profile` in the launch argv, never the network
//! policy, so the plan must prove the entry's existing policy already reaches
//! every profile domain. Coverage uses [`host_allowed`], the matcher the
//! egress proxy enforces with — not the two-way overlap the profile
//! recommender uses.
//!
//! One entry may carry several profiles (#2263): their storage states are
//! merged at launch, so a new profile is only added **alongside** the bound
//! ones when no cookie domain is shared — otherwise the merge would silently
//! pick one identity, and the plan is [`BindPlan::Overlaps`] instead.

use mur_browser::auth::ProfileMeta;
use mur_common::agent::{McpNetMode, McpServerEntry};
use mur_common::net::host_allowed;

use super::live_entry::{attached_profiles, norm, within};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BindPlan {
    /// Every profile domain is reachable under the entry's policy. Safe to
    /// write. `alongside` are the profiles already bound that stay; with
    /// `replace`, `replaces` names the ones dropped instead.
    Bind {
        alongside: Vec<String>,
        replaces: Vec<String>,
    },
    /// A bound profile shares a domain with `site`; adding would merge two
    /// identities for the same host. Never written without `--replace`.
    Overlaps { with: String, domains: Vec<String> },
    /// `Restricted` entry; some profile domains fail `host_allowed`. Never written.
    NeedsHosts {
        current: Vec<String>,
        missing: Vec<String>,
    },
    /// `BroadAudited` entry; some profile domains are in `deny_hosts`. Never written.
    Denied { hosts: Vec<String> },
    /// `network` is `None`, `Inherit`, or `Off`: no host scope to check against.
    NotHostScoped { mode: Option<McpNetMode> },
    /// The agent has no live browser entry.
    NoLiveEntry,
}

/// The hosts a profile is for: its `allow_domains`, or its login URL's host
/// when it has none (the default `mur browser auth` applies). `None` when the
/// metadata names no host at all — such a profile cannot be checked, so it
/// must never reach [`plan_bind`].
pub(crate) fn profile_domains(meta: &ProfileMeta) -> Option<Vec<String>> {
    let domains = if meta.allow_domains.is_empty() {
        vec![url::Url::parse(&meta.url).ok()?.host_str()?.to_owned()]
    } else {
        meta.allow_domains.clone()
    };
    let domains: Vec<String> = domains.iter().map(|d| norm(d)).collect();
    (!domains.is_empty()).then_some(domains)
}

/// The domains of each profile already bound to an entry, looked up by the
/// caller (the planner is pure). A profile whose metadata cannot be read maps
/// to `None`: it cannot be checked for overlap, so adding alongside it is
/// refused with `--replace` as the way out.
pub(crate) type BoundDomains<'a> = dyn Fn(&str) -> Option<Vec<String>> + 'a;

/// Plan attaching profile `site` (whose hosts are `domains`, from
/// [`profile_domains`]) to `entry`. `entry` is the agent's live entry, or
/// `None` when it has none. `bound` resolves the domains of profiles the
/// entry already carries; `replace` drops them instead of adding alongside.
pub(crate) fn plan_bind(
    entry: Option<&McpServerEntry>,
    site: &str,
    domains: &[String],
    bound: &BoundDomains<'_>,
    replace: bool,
) -> BindPlan {
    let Some(entry) = entry else {
        return BindPlan::NoLiveEntry;
    };
    let bind = |entry: &McpServerEntry| bind(entry, site, domains, bound, replace);
    let network = entry.network.as_ref();
    match network.map(|n| n.mode) {
        Some(McpNetMode::Restricted) => {
            let allow = &network.expect("mode came from it").allow_hosts;
            let missing: Vec<String> = domains
                .iter()
                .filter(|d| !host_allowed(d, allow))
                .cloned()
                .collect();
            if missing.is_empty() {
                bind(entry)
            } else {
                BindPlan::NeedsHosts {
                    current: allow.clone(),
                    missing,
                }
            }
        }
        Some(McpNetMode::BroadAudited) => {
            let deny = &network.expect("mode came from it").deny_hosts;
            let hosts: Vec<String> = domains
                .iter()
                .filter(|d| host_allowed(d, deny))
                .cloned()
                .collect();
            if hosts.is_empty() {
                bind(entry)
            } else {
                BindPlan::Denied { hosts }
            }
        }
        mode @ (None | Some(McpNetMode::Inherit) | Some(McpNetMode::Off)) => {
            BindPlan::NotHostScoped { mode }
        }
    }
}

fn bind(
    entry: &McpServerEntry,
    site: &str,
    domains: &[String],
    bound: &BoundDomains<'_>,
    replace: bool,
) -> BindPlan {
    let others: Vec<String> = attached_profiles(entry)
        .into_iter()
        .filter(|old| *old != site)
        .map(str::to_owned)
        .collect();
    if replace {
        return BindPlan::Bind {
            alongside: Vec::new(),
            replaces: others,
        };
    }
    for other in &others {
        let shared = match bound(other) {
            // Unreadable metadata: treat as overlapping everything, since
            // nothing proves the two logins are for different hosts.
            None => domains.to_vec(),
            Some(theirs) => overlap(domains, &theirs),
        };
        if !shared.is_empty() {
            return BindPlan::Overlaps {
                with: other.clone(),
                domains: shared,
            };
        }
    }
    BindPlan::Bind {
        alongside: others,
        replaces: Vec::new(),
    }
}

/// Domains of `a` that a cookie for some domain of `b` would also be sent to
/// (either is the other or a subdomain of it): `deepseek.com` and
/// `chat.deepseek.com` overlap; `deepseek.com` and `gitea.example` do not.
fn overlap(a: &[String], b: &[String]) -> Vec<String> {
    a.iter()
        .filter(|x| b.iter().any(|y| within(x, y) || within(y, x)))
        .cloned()
        .collect()
}

/// One-line reason a plan is not `Bind`, for status warnings. `None` for `Bind`.
pub(crate) fn warning(plan: &BindPlan) -> Option<String> {
    match plan {
        BindPlan::Bind { .. } => None,
        BindPlan::Overlaps { with, domains } => Some(format!(
            "shares {} with bound profile {with:?}",
            domains.join(", ")
        )),
        BindPlan::NeedsHosts { missing, .. } => {
            Some(format!("allow_hosts lacks {}", missing.join(", ")))
        }
        BindPlan::Denied { hosts } => Some(format!("deny_hosts blocks {}", hosts.join(", "))),
        BindPlan::NotHostScoped { mode } => Some(format!(
            "network mode {} is not host-scoped",
            mode.map_or("unset".into(), |m| format!("{m:?}").to_ascii_lowercase())
        )),
        BindPlan::NoLiveEntry => Some("no live browser entry".into()),
    }
}

#[cfg(test)]
#[path = "bind_plan_tests.rs"]
mod tests;
