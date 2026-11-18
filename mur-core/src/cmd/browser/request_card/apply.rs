//! Apply the user's answer to a browser request card. Every write is bounded
//! by the card (D11): it lands only through `save_if_unchanged` against the
//! entries the card was built from, and it adds only hosts the card named.
//! The request is removed only once the answer is applied (D8).

use std::path::Path;

use anyhow::Result;
use mur_browser_request::PendingRequest;
use mur_common::agent::{McpNetMode, McpServerEntry};

use super::super::bind_plan::{BindPlan, BoundDomains, plan_bind_for, plan_reach, warning};
use super::super::live_effect::Effect;
use super::super::live_entry::{LIVE_ENTRY, LiveOps, attached_profiles, norm, set_profiles};
use super::model::{Offer, add_hosts};

/// The agent's outbound port grant, injected so tests run with a fake.
/// `Ok` carries the line to show (the same NOTE `perm allow-port` prints).
pub(crate) trait PortOps {
    fn open(&mut self, port: u16) -> Result<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Choice {
    /// A profile row (built by [`use_offer`]). `domains` are the row's;
    /// `approved` the hosts the row said it adds; `replace` the explicit
    /// choice a `ReplaceOnly` row needs.
    Use {
        profile: String,
        domains: Vec<String>,
        replace: bool,
        approved: Vec<String>,
    },
    /// A reach card's "allow" (D7). `approved` as for `Use`.
    Allow {
        approved: Vec<String>,
    },
    Dismiss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// Applied; the request is gone. `effect` is `None` when nothing was
    /// written (D12).
    Done {
        effect: Option<Effect>,
        notes: Vec<String>,
        hand_back: Option<String>,
    },
    /// The plan does not allow it; nothing written, request kept.
    Refused(String),
    /// The entries changed since the card was read, or the answer would add
    /// a host the card did not name; nothing written, request kept.
    Stale,
    /// The binding was saved (`effect`, `notes`), the port grant was not;
    /// request kept so it can be answered again (council N6).
    PortFailed {
        effect: Option<Effect>,
        notes: Vec<String>,
        error: String,
    },
}

pub(crate) struct Answer<'a> {
    pub agent: &'a str,
    pub agent_home: &'a Path,
    pub request: &'a PendingRequest,
    /// The card's port step, if it had one.
    pub port: Option<u16>,
    /// The MCP entries the card was built from (D11).
    pub snapshot: &'a [McpServerEntry],
    /// A card login just saved this profile's state (D12).
    pub login_saved: Option<&'a str>,
}

/// The choice a row allows, or why it allows none. The only way the CLI and
/// slice 4's buttons build a `Choice::Use`.
pub(crate) fn use_offer(
    profile: &str,
    domains: &[String],
    offer: &Offer,
    replace: bool,
) -> Result<Choice, String> {
    let needs_replace = matches!(offer, Offer::ReplaceOnly { .. });
    match offer {
        Offer::Disabled { why } => return Err(format!("not available: {why}")),
        _ if needs_replace && !replace => {
            return Err(format!(
                "{profile:?} only binds as a replacement: add --replace"
            ));
        }
        _ if replace && !needs_replace => {
            return Err(format!("{profile:?} needs no --replace; drop it"));
        }
        _ => {}
    }
    Ok(Choice::Use {
        profile: profile.to_owned(),
        domains: domains.to_vec(),
        replace,
        approved: offer.approved().to_vec(),
    })
}

enum Refusal {
    Plan(String),
    Stale,
}

pub(crate) fn answer(
    a: &Answer<'_>,
    choice: &Choice,
    open_port: bool,
    bound: &BoundDomains<'_>,
    ops: &mut dyn LiveOps,
    ports: &mut dyn PortOps,
) -> Result<Answered> {
    if *choice == Choice::Dismiss {
        mur_browser_request::remove(a.agent_home, a.request)?;
        return Ok(Answered::Done {
            effect: None,
            notes: Vec::new(),
            hand_back: None,
        });
    }
    // Checked here for an answer that writes nothing; a write is checked
    // again by `save_if_unchanged`, under the profile lock.
    if ops.servers()? != a.snapshot {
        return Ok(Answered::Stale);
    }
    let mut servers = a.snapshot.to_vec();
    let Some(entry) = servers.iter_mut().find(|e| e.name == LIVE_ENTRY) else {
        return Ok(Answered::Refused(
            warning(&BindPlan::NoLiveEntry).unwrap_or_default(),
        ));
    };
    let host = norm(a.request.site.host());
    let (profile, record) = match choice {
        Choice::Use {
            profile,
            domains,
            replace,
            approved,
        } => {
            let before = attached_profiles(entry).contains(&profile.as_str());
            let relogin = before && a.login_saved == Some(profile.as_str());
            match bind_into(entry, profile, domains, &host, *replace, approved, bound) {
                Ok(dropped) => {
                    let rebound = !before || dropped.is_some() || relogin;
                    (
                        Some(profile.as_str()),
                        rebound.then_some((dropped, relogin)),
                    )
                }
                Err(Refusal::Plan(why)) => return Ok(Answered::Refused(why)),
                Err(Refusal::Stale) => return Ok(Answered::Stale),
            }
        }
        Choice::Allow { approved } => match allow_into(entry, &host, approved) {
            Ok(()) => (None, None),
            Err(Refusal::Plan(why)) => return Ok(Answered::Refused(why)),
            Err(Refusal::Stale) => return Ok(Answered::Stale),
        },
        Choice::Dismiss => unreachable!("handled above"),
    };
    let (mut notes, mut effect) = (Vec::new(), None);
    if servers != a.snapshot {
        match ops.save_if_unchanged(a.snapshot, servers)? {
            Some((n, e)) => (notes, effect) = (n, Some(e)),
            None => return Ok(Answered::Stale),
        }
    }
    if let (Some(p), Some((dropped, relogin))) = (profile, record) {
        if relogin {
            effect = Some(Effect::Restart);
        }
        // Telemetry must not undo a write that already succeeded.
        if let Err(e) = ops.record_profile_bound(a.agent, dropped.as_deref(), p) {
            tracing::warn!("browser card: profile_bound trace not recorded: {e:#}");
        }
    }
    if open_port && let Some(p) = a.port {
        match ports.open(p) {
            Ok(note) => {
                notes.push(note);
                effect = Some(Effect::Restart);
            }
            Err(e) => {
                return Ok(Answered::PortFailed {
                    effect,
                    notes,
                    error: format!("{e:#}"),
                });
            }
        }
    }
    mur_browser_request::remove(a.agent_home, a.request)?;
    Ok(Answered::Done {
        effect,
        notes,
        hand_back: Some(hand_back_text(a.request, profile)),
    })
}

/// Bind `profile` into `entry` per the plan, adding only `approved` hosts.
/// The plan comes first (council B2). An already attached profile is left
/// where it is. `Ok(Some(old))` names the profiles a replace dropped.
fn bind_into(
    entry: &mut McpServerEntry,
    profile: &str,
    domains: &[String],
    host: &str,
    replace: bool,
    approved: &[String],
    bound: &BoundDomains<'_>,
) -> Result<Option<String>, Refusal> {
    let reach = [host.to_owned()];
    let plan =
        |e: &McpServerEntry| plan_bind_for(Some(e), profile, domains, &reach, bound, replace);
    let BindPlan::Bind {
        mut alongside,
        replaces,
    } = add_approved(entry, approved, plan)?
    else {
        unreachable!("add_approved returns only Bind");
    };
    if replace || !attached_profiles(entry).contains(&profile) {
        alongside.push(profile.to_owned());
        set_profiles(entry, &alongside);
    }
    Ok((!replaces.is_empty()).then(|| replaces.join(",")))
}

fn allow_into(entry: &mut McpServerEntry, host: &str, approved: &[String]) -> Result<(), Refusal> {
    match add_approved(entry, approved, |e| plan_reach(Some(e), host)) {
        Ok(_) => Ok(()),
        // D7: unset and inherit are host-open; the agent's policy decides.
        Err(Refusal::Plan(_))
            if matches!(
                plan_reach(Some(&*entry), host),
                BindPlan::NotHostScoped {
                    mode: None | Some(McpNetMode::Inherit)
                }
            ) =>
        {
            Ok(())
        }
        Err(r) => Err(r),
    }
}

/// Plan; if it only lacks hosts the card approved, add them and plan again.
/// Returns the final `Bind`, or why there is none. A host the card did not
/// name is `Stale`: the entry is no longer what the user approved.
fn add_approved(
    entry: &mut McpServerEntry,
    approved: &[String],
    plan: impl Fn(&McpServerEntry) -> BindPlan,
) -> Result<BindPlan, Refusal> {
    let mut p = plan(entry);
    if let BindPlan::NeedsHosts { missing, .. } = &p {
        if !missing.iter().all(|h| approved.contains(h)) {
            return Err(Refusal::Stale);
        }
        let missing = missing.clone();
        add_hosts(entry, &missing);
        p = plan(entry);
    }
    match p {
        BindPlan::Bind { .. } => Ok(p),
        other => Err(Refusal::Plan(warning(&other).unwrap_or_default())),
    }
}

/// What resumes the agent after its request is answered (D9). Its first
/// visit is the test, and the user sees the result as agent-reported
/// (S:§The card).
pub(crate) fn hand_back_text(r: &PendingRequest, profile: Option<&str>) -> String {
    let with = profile
        .map(|p| format!(" with the saved login {p:?}"))
        .unwrap_or_default();
    format!(
        "Browser access to {} is set up{with} (you asked: \"{}\"). Your first visit is \
         the test: open the site and report what you see. The user sees your report as \
         agent-reported, not as a MUR check.",
        r.site.origin(),
        r.reason
    )
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
