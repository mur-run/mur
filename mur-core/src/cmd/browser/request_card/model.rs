//! The card, decided without I/O: which saved profiles match the request's
//! host, and what the planner says each would do to the live entry
//! (S:§The card table). Every `BindPlan` variant maps to exactly one
//! [`Offer`]; nothing here writes.

use mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS;
use mur_browser_request::Site;
use mur_common::agent::{McpNetMode, McpServerEntry, NetworkOutboundMode};

use super::super::bind_plan::{BindPlan, BoundDomains, plan_bind_for, plan_reach, warning};
use super::super::live_entry::{attached_profiles, norm, within};

/// A saved profile with a usable host list (`profile_domains` is `Some`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Profile {
    pub name: String,
    pub domains: Vec<String>,
    /// The host of the page the profile logged in at (`meta.url`), when
    /// readable.
    pub login_host: Option<String>,
}

/// What picking a profile row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// The live entry already launches with it and reaches every host:
    /// nothing to write.
    AlreadyBound,
    /// `BindPlan::Bind`: added alongside the bound profiles.
    Offered,
    /// `BindPlan::NeedsHosts`: picking it adds `missing` in the same save.
    AddsHosts { missing: Vec<String> },
    /// `BindPlan::Overlaps`: only with the explicit replace choice, which
    /// drops every profile in `drops` and adds `adds` (D4).
    ReplaceOnly {
        with: String,
        domains: Vec<String>,
        drops: Vec<String>,
        adds: Vec<String>,
    },
    /// `Denied`, `NotHostScoped`: listed, never written. `why` is the
    /// planner's own warning.
    Disabled { why: String },
}

impl Offer {
    /// The hosts picking this row may add. The answer adds no other (D11).
    pub(crate) fn approved(&self) -> &[String] {
        match self {
            Offer::AddsHosts { missing } => missing,
            Offer::ReplaceOnly { adds, .. } => adds,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRow {
    pub name: String,
    /// The profile's hosts, as planned; the answer binds with exactly these.
    pub domains: Vec<String>,
    /// Other agents whose live entry already launches with this profile.
    pub shared_with: Vec<String>,
    pub offer: Offer,
    /// Where the profile logged in, when no one of its domains covers both
    /// that host and the request's (#2461): its saved session is for another
    /// site, so the card does not default to it.
    pub session_elsewhere: Option<String>,
}

/// A `login: false` request (D7): can the live entry reach the host?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    Allowed,
    NeedsHost {
        missing: Vec<String>,
    },
    /// The entry is not host-scoped (`unset`/`inherit`): the agent's own
    /// network policy decides; nothing to write.
    Unscoped {
        mode: String,
    },
    /// `off`, or `deny_hosts` blocks the host.
    Disabled {
        why: String,
    },
}

/// The profile "log in again" saves under (D6), and whether one of that
/// name exists already (then `--reauth` is needed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginAgain {
    pub name: String,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Card {
    /// The agent has no live browser entry; creating one stays with
    /// `/browser live` (S:§The card, `NoLiveEntry`).
    NoLiveEntry,
    Login {
        rows: Vec<ProfileRow>,
        login_again: LoginAgain,
        /// What binding the "log in again" profile would do, decided the
        /// way the login itself decides it (#2461): a `ReplaceOnly` login
        /// needs the explicit replace choice too.
        login_offer: Offer,
        port: Option<u16>,
    },
    Reach {
        reach: Reach,
        port: Option<u16>,
    },
}

/// What the card is built from; `ops::card_for` reads it off disk.
pub(crate) struct Inputs<'a> {
    pub entry: Option<&'a McpServerEntry>,
    pub profiles: &'a [Profile],
    pub bound: &'a BoundDomains<'a>,
    pub shared: &'a dyn Fn(&str) -> Vec<String>,
    pub port: Option<u16>,
    /// The hosts the login would save with (`login_plan`), or why no
    /// login can be planned.
    pub login_domains: Result<Vec<String>, String>,
}

/// D1: the host is a profile domain or a label-aligned subdomain of one,
/// the rule the profile's own navigation guard applies. Listing only:
/// reaching the host is the planner's check.
fn matches(host: &str, p: &Profile) -> bool {
    p.domains.iter().any(|d| within(host, d))
}

/// The profile's login host, when none of its domains covers both it and
/// `host`: e.g. a `vercel` profile that lists `github.com` but logged in at
/// `vercel.com` holds no GitHub session. A same-site SSO host
/// (`login.example.com` for `app.example.com` under `example.com`) is not
/// flagged. Unknown login host: not flagged.
fn session_elsewhere(host: &str, p: &Profile) -> Option<String> {
    let at = norm(p.login_host.as_deref()?);
    let same = p.domains.iter().any(|d| within(host, d) && within(&at, d));
    (!same).then_some(at)
}

/// The port the card offers to open (D5): non-zero, non-default, outside
/// the web set, not granted yet, and only where a port grant has effect
/// (`restricted`).
pub(crate) fn port_step(site: &Site, mode: NetworkOutboundMode, open: &[u16]) -> Option<u16> {
    let p = site.port()?;
    (p != 0
        && mode == NetworkOutboundMode::Restricted
        && !RESTRICTED_GENERAL_PORTS.contains(&p)
        && !open.contains(&p))
    .then_some(p)
}

pub(crate) fn build(site: &Site, login: bool, login_again: LoginAgain, i: &Inputs<'_>) -> Card {
    let Some(entry) = i.entry else {
        return Card::NoLiveEntry;
    };
    let host = norm(site.host());
    if !login {
        return Card::Reach {
            reach: reach(entry, &host),
            port: i.port,
        };
    }
    let rows = i
        .profiles
        .iter()
        .filter(|p| matches(&host, p))
        .map(|p| ProfileRow {
            name: p.name.clone(),
            domains: p.domains.clone(),
            shared_with: (i.shared)(&p.name),
            offer: offer_for(entry, p, &host, i.bound),
            session_elsewhere: session_elsewhere(&host, p),
        })
        .collect();
    let login_offer = match &i.login_domains {
        Ok(domains) => {
            let p = Profile {
                name: login_again.name.clone(),
                domains: domains.clone(),
                login_host: None,
            };
            offer_for(entry, &p, &host, i.bound)
        }
        Err(why) => Offer::Disabled { why: why.clone() },
    };
    Card::Login {
        rows,
        login_again,
        login_offer,
        port: i.port,
    }
}

/// What picking `p` would do for a request to `host`. The plan comes first:
/// an attached profile is `AlreadyBound` only when the plan is a plain
/// `Bind` (council B2). The request host is checked for coverage (D1). The
/// `--login` preflight calls this too, so a sign-in is never spent on a
/// row that would be refused.
pub(crate) fn offer_for(
    entry: &McpServerEntry,
    p: &Profile,
    host: &str,
    bound: &BoundDomains<'_>,
) -> Offer {
    let reach = [host.to_owned()];
    let reach = &reach[..];
    let plan = |replace: bool| {
        move |e: &McpServerEntry| plan_bind_for(Some(e), &p.name, &p.domains, reach, bound, replace)
    };
    let (missing, settled) = settle(entry, plan(false));
    match settled {
        BindPlan::Bind { .. } if !missing.is_empty() => Offer::AddsHosts { missing },
        BindPlan::Bind { .. } if attached_profiles(entry).contains(&p.name.as_str()) => {
            Offer::AlreadyBound
        }
        BindPlan::Bind { .. } => Offer::Offered,
        BindPlan::Overlaps { with, domains } => match settle(entry, plan(true)) {
            (adds, BindPlan::Bind { replaces, .. }) => Offer::ReplaceOnly {
                with,
                domains,
                drops: replaces,
                adds,
            },
            (_, other) => disabled(&other),
        },
        other => disabled(&other),
    }
}

/// Plan, and when the plan only lacks hosts, plan again with them added, so
/// the card shows what the write would meet. For example, an overlap that
/// shows up only after the hosts are added is `ReplaceOnly`, not
/// `AddsHosts`. Returns the hosts it added.
fn settle(
    entry: &McpServerEntry,
    plan: impl Fn(&McpServerEntry) -> BindPlan,
) -> (Vec<String>, BindPlan) {
    let first = plan(entry);
    let BindPlan::NeedsHosts { missing, .. } = first else {
        return (Vec::new(), first);
    };
    let mut e = entry.clone();
    add_hosts(&mut e, &missing);
    (missing, plan(&e))
}

fn disabled(plan: &BindPlan) -> Offer {
    Offer::Disabled {
        why: warning(plan).unwrap_or_default(),
    }
}

fn reach(entry: &McpServerEntry, host: &str) -> Reach {
    match settle(entry, |e| plan_reach(Some(e), host)) {
        (missing, BindPlan::Bind { .. }) if missing.is_empty() => Reach::Allowed,
        (missing, BindPlan::Bind { .. }) => Reach::NeedsHost { missing },
        (_, BindPlan::NotHostScoped { mode: None }) => Reach::Unscoped {
            mode: "unset".into(),
        },
        (
            _,
            BindPlan::NotHostScoped {
                mode: Some(McpNetMode::Inherit),
            },
        ) => Reach::Unscoped {
            mode: "inherit".into(),
        },
        (_, plan) => Reach::Disabled {
            why: warning(&plan).unwrap_or_default(),
        },
    }
}

/// Append to a `restricted` entry's `allow_hosts`, keeping its order. Any
/// other mode is left alone (the planner never asks for hosts there).
pub(crate) fn add_hosts(entry: &mut McpServerEntry, hosts: &[String]) {
    let Some(net) = entry
        .network
        .as_mut()
        .filter(|n| n.mode == McpNetMode::Restricted)
    else {
        return;
    };
    for h in hosts {
        if !net.allow_hosts.contains(h) {
            net.allow_hosts.push(h.clone());
        }
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
