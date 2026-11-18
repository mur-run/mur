//! The card's inputs read off disk, and the real port grant. Best-effort
//! like `mur browser status`: an unreadable profile is not offered, an
//! unreadable agent is not listed as sharing.

use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use mur_browser::paths;
use mur_browser_request::{PendingRequest, read_pending};
use mur_common::agent::McpServerEntry;

use super::super::bind::bound_domains;
use super::super::bind_plan::profile_domains;
use super::super::live_entry::{LIVE_ENTRY, attached_profiles};
use super::apply::PortOps;
use super::login::{default_name, login_plan};
use super::model::{self, Card, Inputs, LoginAgain, Profile};

/// The MCP entries a card was built from (D11). The answer writes only if
/// they are still what the profile holds.
pub(crate) type Snapshot = Vec<McpServerEntry>;

/// Every saved profile with state and a usable host list, by name.
pub(crate) fn saved_profiles(home: &Path) -> Vec<Profile> {
    let Ok(dir) = std::fs::read_dir(paths::browser_root(home).join("profiles")) else {
        return Vec::new();
    };
    let mut out: Vec<Profile> = dir
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| paths::validate_name(name).is_ok())
        .filter(|name| paths::profile_state(home, name).is_file())
        .filter_map(|name| {
            let meta = super::super::read_profile_meta(&paths::profile_meta(home, &name)).ok()?;
            Some(Profile {
                domains: profile_domains(&meta)?,
                login_host: url::Url::parse(&meta.url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_owned)),
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Every agent's live browser entry, by agent. One profile load per agent:
/// build it once and pass it to [`card_for_in`] for many requests.
pub(crate) type LiveEntries = Vec<(String, McpServerEntry)>;

pub(crate) fn live_entries(home: &Path) -> LiveEntries {
    crate::a2a_dial::agent_dir_names(home)
        .into_iter()
        .filter_map(|agent| {
            let (_, p) = crate::cmd::agent::load_profile_at(home, &agent).ok()?;
            let e = p.mcp_servers.into_iter().find(|e| e.name == LIVE_ENTRY)?;
            Some((agent, e))
        })
        .collect()
}

pub(crate) fn open_requests(home: &Path, agent: &str) -> Vec<PendingRequest> {
    read_pending(&home.join("agents").join(agent), Utc::now())
}

/// Other agents whose live entry launches with `profile`.
pub(crate) fn shared_with(home: &Path, agent: &str, profile: &str) -> Vec<String> {
    sharing(&live_entries(home), agent, profile)
}

fn sharing(entries: &[(String, McpServerEntry)], agent: &str, profile: &str) -> Vec<String> {
    entries
        .iter()
        .filter(|(a, e)| a != agent && attached_profiles(e).contains(&profile))
        .map(|(a, _)| a.clone())
        .collect()
}

pub(crate) fn card_for(home: &Path, agent: &str, r: &PendingRequest) -> Result<(Card, Snapshot)> {
    card_for_in(home, &live_entries(home), agent, r)
}

/// [`card_for`] against already-loaded [`live_entries`].
pub(crate) fn card_for_in(
    home: &Path,
    entries: &[(String, McpServerEntry)],
    agent: &str,
    r: &PendingRequest,
) -> Result<(Card, Snapshot)> {
    let (_, profile) = crate::cmd::agent::load_profile_at(home, agent)?;
    let entry = profile.mcp_servers.iter().find(|e| e.name == LIVE_ENTRY);
    let out = &profile.entitlements.network.outbound;
    let shared = |name: &str| sharing(entries, agent, name);
    let profiles = saved_profiles(home);
    let bound = bound_domains(home);
    let name = default_name(&r.site);
    let exists = paths::profile_state(home, &name).exists();
    // The same plan `l` makes, so the card offers what the login would do.
    let login_domains = login_plan(home, &r.site, Some(&name), exists)
        .map(|p| p.allow_domains)
        .map_err(|e| format!("{e:#}"));
    let login_again = LoginAgain { exists, name };
    let card = model::build(
        &r.site,
        r.login,
        login_again,
        &Inputs {
            entry,
            profiles: &profiles,
            bound: &bound,
            shared: &shared,
            port: model::port_step(&r.site, out.mode, &out.allow_ports),
            login_domains,
        },
    );
    Ok((card, profile.mcp_servers.clone()))
}

/// `mur agent perm allow-port`'s writer (D5): one code path, one NOTE.
pub(crate) struct AgentPorts<'a>(pub &'a str);

impl PortOps for AgentPorts<'_> {
    fn open(&mut self, port: u16) -> Result<String> {
        Ok(crate::cmd::agent::grant_port(self.0, port)?
            .unwrap_or_else(|| format!("port {port} is already open by default")))
    }
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
