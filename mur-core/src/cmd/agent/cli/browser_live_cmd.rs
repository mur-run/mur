//! `/browser live <host>...`: persist a live-mode browser MCP entry on this
//! agent and scope its egress to exactly the given hosts.
//!
//! MCP has no hot reload, so the entry only takes effect after a restart; the
//! restart is offered as a chip, never run automatically (it would end the
//! very session that typed the command).
//!
//! The decision logic is pure ([`parse_hosts`], [`apply`]) so tests never
//! touch a profile on disk or spawn the install probe `manage::mcp_add` runs.

use mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS;
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};
use mur_common::proposal::Proposal;

use super::manage::{self, Managed};

/// Profile entry name for the live browser server.
pub(super) const LIVE_ENTRY: &str = "browser";

/// Example shown when no host was given.
pub(super) const USAGE_EXAMPLE: &str = "/browser live shop-a.example.com";

/// Where `/browser` should go for a given argument list.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// `--add`: attach the skill (unchanged legacy behavior).
    Add,
    /// `live ...`: the remaining args are the hosts.
    Live(Vec<String>),
    /// Anything else: forwarded to the model as a browser-skill turn.
    Turn,
}

pub(super) fn route(args: &[String]) -> Route {
    match args.first().map(String::as_str) {
        Some("--add") => Route::Add,
        Some("live") => Route::Live(args[1..].to_vec()),
        _ => Route::Turn,
    }
}

/// Validate and normalize hosts. `host:443` → `host`; a port outside the
/// restricted-mode web set is rejected (see #1619). Error text is user-facing.
pub(super) fn parse_hosts(args: &[String]) -> Result<Vec<String>, String> {
    if args.is_empty() {
        return Err(format!(
            "usage: /browser live <host>... — name the sites this agent may reach, e.g. `{USAGE_EXAMPLE}`"
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for raw in args {
        let (host, port) = match raw.rsplit_once(':') {
            Some((_, "")) => {
                return Err(format!(
                    "`{raw}`: the port after `:` is empty — drop the colon or give a port"
                ));
            }
            Some((h, p)) => {
                let port: u16 = p
                    .parse()
                    .map_err(|_| format!("`{raw}`: port is not a number"))?;
                (h, Some(port))
            }
            None => (raw.as_str(), None),
        };
        // `-` first: `/browser live` parses no flags, so `--add` here is a
        // mistake, never a hostname to put in the allowlist.
        if host.is_empty() || host.contains('/') || host.starts_with('-') {
            return Err(format!(
                "`{raw}` is not a host — pass a bare hostname, e.g. `{USAGE_EXAMPLE}`"
            ));
        }
        if let Some(p) = port
            && !RESTRICTED_GENERAL_PORTS.contains(&p)
        {
            // #1619: auto-opening the port is unsafe until Chromium is shown
            // not to bypass the proxy, so hand the choice back to the user.
            return Err(format!(
                "port {p} is not in restricted mode's web set {RESTRICTED_GENERAL_PORTS:?}; \
                 nothing was changed. Open it yourself with `mur agent perm allow-port <agent> {p}` \
                 (see #1619), then rerun `/browser live {host}`"
            ));
        }
        let host = host.to_ascii_lowercase();
        if !out.contains(&host) {
            out.push(host);
        }
    }
    Ok(out)
}

/// What [`apply`] changed, for the summary line.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Applied {
    /// The live entry was newly created (false = already present, reused).
    pub added: bool,
    /// The allowlist this call replaced (empty if none).
    pub previous: Vec<String>,
}

/// Ensure the live entry exists and overwrite its allowlist with `hosts`.
/// `new_entry` is only called when the entry is missing.
pub(super) fn apply(
    servers: &mut Vec<McpServerEntry>,
    hosts: &[String],
    new_entry: impl FnOnce() -> McpServerEntry,
) -> Applied {
    let added = !servers.iter().any(|e| e.name == LIVE_ENTRY);
    if added {
        servers.push(new_entry());
    }
    let entry = servers
        .iter_mut()
        .find(|e| e.name == LIVE_ENTRY)
        .expect("entry ensured above");
    let previous = entry
        .network
        .take()
        .map(|n| n.allow_hosts)
        .unwrap_or_default();
    // Replace, never merge: the allowlist is exactly what this call named.
    entry.network = Some(McpServerNetwork {
        mode: McpNetMode::Restricted,
        allow_hosts: hosts.to_vec(),
        ..Default::default()
    });
    Applied { added, previous }
}

/// Gate on `mur browser setup`: refuse (and name the command) when not ready.
pub(super) fn check_setup(ready: bool) -> Result<(), String> {
    if ready {
        Ok(())
    } else {
        Err(
            "the browser is not installed — run `mur browser setup --yes` in a terminal \
             first (it needs npm and the registry, which an agent's seal does not grant)"
                .into(),
        )
    }
}

/// `--run` for the live entry: `live-<agent>`. Agent names are already
/// restricted to `[A-Za-z0-9_-]` (`validate_agent_name`), so no sanitizing;
/// per-agent so two agents never share a run even if runs are global.
pub(super) fn live_argv(agent: &str) -> Vec<String> {
    // Order matters: `--run` before `--mode`, as `cli/actions.rs` declares.
    [
        "browser",
        "record",
        "--run",
        &format!("live-{agent}"),
        "--mode",
        "live",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Chip label for the restart live mode needs.
pub(super) const RESTART_LABEL: &str = "restart to enable browser live mode";

/// Profile side effects, injected so tests run with a fake.
pub(super) trait LiveOps {
    /// Current MCP entries on the agent.
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>>;
    /// Create the entry through `manage::mcp_add` (pins + probes + saves).
    fn mcp_add(&mut self, argv: &[String]) -> anyhow::Result<String>;
    /// Persist `servers` back to the profile.
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<()>;
    /// Drop the named entry (rollback of an entry this call created).
    fn remove(&mut self, name: &str) -> anyhow::Result<()>;
}

/// The whole `/browser live` flow. Every refusal returns before the first
/// write, so "refused" always means "nothing changed, no chip".
pub(super) fn run(
    agent: &str,
    args: &[String],
    ready: bool,
    ops: &mut impl LiveOps,
) -> anyhow::Result<Managed> {
    let hosts = parse_hosts(args).map_err(anyhow::Error::msg)?;
    check_setup(ready).map_err(anyhow::Error::msg)?;
    let mut notes = Vec::new();
    // `mcp_add` bails on a duplicate name by contract; decide here instead.
    let created_here = !ops.servers()?.iter().any(|e| e.name == LIVE_ENTRY);
    if created_here {
        notes.push(ops.mcp_add(&live_argv(agent))?);
    }
    // Re-read, not redundant: `mcp_add` saved the profile itself, and the
    // list we save below must include the entry it just wrote.
    let mut servers = ops.servers()?;
    let out = apply(&mut servers, &hosts, || unreachable!("added above"));
    if let Err(save_err) = ops.save(servers) {
        // `mcp_add` saved the entry with no network policy. If this call made
        // it, undo that rather than leave an unrestricted `browser` entry. A
        // pre-existing entry keeps its old (still valid) policy: the save is
        // atomic (`save_profile` → `write_atomic`), so a failed save changed
        // nothing.
        if created_here && let Err(rm_err) = ops.remove(LIVE_ENTRY) {
            return Err(save_err.context(format!(
                "the '{LIVE_ENTRY}' entry was created but could not be removed \
                 ({rm_err:#}); check the profile by hand: it may have no \
                 network allowlist"
            )));
        }
        return Err(save_err.context("could not save the browser live allowlist"));
    }
    notes.push(format!("browser live mode may reach: {}", hosts.join(", ")));
    if !out.previous.is_empty() && out.previous != hosts {
        notes.push(format!(
            "replaced previous allowlist: {}",
            out.previous.join(", ")
        ));
    }
    let (text, _) = manage::applied(&notes.join("\n"));
    Ok((text, Some(Proposal::restart(RESTART_LABEL))))
}

/// Real profile-backed [`LiveOps`].
pub(super) struct ProfileOps<'a>(pub &'a str);

impl LiveOps for ProfileOps<'_> {
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>> {
        Ok(crate::cmd::agent::load_profile_for_edit(self.0)?
            .1
            .mcp_servers)
    }
    fn mcp_add(&mut self, argv: &[String]) -> anyhow::Result<String> {
        Ok(manage::mcp_add(self.0, LIVE_ENTRY, "mur", argv)?.0)
    }
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<()> {
        let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(self.0)?;
        profile.mcp_servers = servers;
        crate::cmd::agent::save_profile(&path, &mut profile)
    }
    fn remove(&mut self, name: &str) -> anyhow::Result<()> {
        manage::mcp_remove(self.0, name).map(|_| ())
    }
}

#[cfg(test)]
#[path = "browser_live_cmd_tests.rs"]
mod tests;
