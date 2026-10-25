//! `/browser allow [--session] [--yes] <host>` and `/browser status` (#1906,
//! #1907). The command forms of the 永久放行 / 這次放行 chips, and the text
//! fallback any non-TUI client can paste.
//!
//! - `--session`: in-memory on the running proxy only; nothing is written and
//!   there is no preview (spec Part 3, 這次放行).
//! - default: Part 1 validation → Part 2 preview → on `--yes`, write to the
//!   profile AND apply live, so the proxy needs no restart (永久放行).
//! - A `BroadAudited` server refuses: every host but the deny list is already
//!   reachable, and a deny-listed host is never grantable (D3).

use mur_agent_runtime::sandbox::egress_proxy::DENY_OVERLAY_ENFORCEMENT;
use mur_common::agent::{McpNetMode, McpServerEntry};
use mur_common::host_pattern::normalize_host_pattern;
use mur_common::proposal::Proposal;

use super::browser_live_cmd::LIVE_ENTRY;
use super::browser_live_preview::render_preview;
use super::browser_profile_hint::{self as profile_hint, SavedProfile};
use super::manage::Managed;

pub(super) const ALLOW_USAGE: &str =
    "usage: /browser allow [--session] <host> — let the browser reach one more host";

/// Parsed `/browser allow` arguments.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct AllowArgs {
    pub host: String,
    pub session: bool,
    pub yes: bool,
}

pub(super) fn parse_allow(args: &[String]) -> Result<AllowArgs, String> {
    let (mut session, mut yes, mut hosts) = (false, false, Vec::new());
    for a in args {
        match a.as_str() {
            "--session" => session = true,
            "--yes" => yes = true,
            _ => hosts.push(a.as_str()),
        }
    }
    let [host] = hosts.as_slice() else {
        return Err(ALLOW_USAGE.to_owned());
    };
    let n = normalize_host_pattern(host)
        .map_err(|e| format!("`{host}` is not a host: {e} — nothing was changed"))?;
    Ok(AllowArgs {
        host: n.value,
        session,
        yes,
    })
}

/// Side effects, injected so tests never touch a profile or a socket.
pub(super) trait AllowOps {
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>>;
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<()>;
    /// Dial `egress/grant` on the running agent. `Err` text is shown as-is.
    fn grant_live(&mut self, host: &str, session: bool) -> Result<(), String>;
    /// Saved `mur browser auth` profiles, for the #2100 hint. Best-effort.
    fn saved_profiles(&mut self) -> Vec<SavedProfile>;
}

fn confirm_chip(host: &str) -> Option<Proposal> {
    let cmd = format!("browser allow --yes {host}");
    let args = serde_json::json!({
        "label": format!("always allow {host}"),
        "kind": "slash",
        "command": cmd,
    });
    mur_common::proposal::vet(&args).ok()
}

pub(super) fn run_allow(
    agent: &str,
    args: &[String],
    ops: &mut impl AllowOps,
) -> anyhow::Result<Managed> {
    let a = parse_allow(args).map_err(anyhow::Error::msg)?;
    let mut servers = ops.servers()?;
    let Some(entry) = servers.iter().find(|e| e.name == LIVE_ENTRY) else {
        anyhow::bail!(
            "this agent has no `{LIVE_ENTRY}` server yet — set one up with `/browser live <host>`"
        );
    };
    let before = entry.network.clone().unwrap_or_default();
    let entry = entry.clone();
    match before.mode {
        McpNetMode::Restricted => {}
        McpNetMode::BroadAudited => anyhow::bail!(
            "`{LIVE_ENTRY}` is in research mode: every host except its deny list is already \
             reachable, and a deny-listed host cannot be granted from here"
        ),
        other => anyhow::bail!(
            "`{LIVE_ENTRY}` is not host-scoped (mode {other:?}); `/browser live <host>` scopes it first"
        ),
    }
    let mut reachable = before.allow_hosts.clone();
    if !reachable.contains(&a.host) {
        reachable.push(a.host.clone());
    }
    if a.session {
        return match ops.grant_live(&a.host, true) {
            Ok(()) => Ok(with_profile_hint(
                format!(
                    "`{}` allowed for this session — gone when the agent restarts. \
                     Retry the page; the blocked request itself is not replayed.",
                    a.host
                ),
                &entry,
                &reachable,
                ops,
            )),
            Err(e) => anyhow::bail!("could not allow `{}` on the running agent: {e}", a.host),
        };
    }
    if before.allow_hosts.contains(&a.host) {
        return Ok(with_profile_hint(
            format!(
                "`{}` is already on the browser's allow-list — nothing changed",
                a.host
            ),
            &entry,
            &reachable,
            ops,
        ));
    }
    let mut after = before.clone();
    after.allow_hosts.push(a.host.clone());
    let preview = render_preview(
        agent,
        LIVE_ENTRY,
        Some(&before),
        &after,
        &[],
        DENY_OVERLAY_ENFORCEMENT,
    )
    // A live grant takes effect now; the generic tail says "after restart".
    .replace(
        "Saved to the profile; takes effect after the agent restarts.",
        "Saved to the profile and applied to the running agent now.",
    );
    if !a.yes {
        return Ok(match confirm_chip(&a.host) {
            Some(chip) => (preview, Some(chip)),
            None => (
                format!("{preview}\nRun: `/browser allow --yes {}`", a.host),
                None,
            ),
        });
    }
    if let Some(e) = servers.iter_mut().find(|e| e.name == LIVE_ENTRY) {
        e.network = Some(after);
    }
    ops.save(servers)?;
    let live = match ops.grant_live(&a.host, false) {
        Ok(()) => "applied to the running agent now".to_owned(),
        // The profile write stands; only the live half missed.
        Err(e) => {
            format!("saved, but the running agent was not reached ({e}) — restart it to apply")
        }
    };
    Ok(with_profile_hint(
        format!("`{}` always allowed: {live}", a.host),
        &entry,
        &reachable,
        ops,
    ))
}

/// Append the #2100 hint: the host is reachable now, but if the entry has
/// no `--profile` and a saved login covers it, the page will still be the
/// logged-out one. Only outcome paths call this, never the preview, so the
/// confirm chip there is never displaced.
fn with_profile_hint(
    text: String,
    entry: &McpServerEntry,
    hosts: &[String],
    ops: &mut impl AllowOps,
) -> Managed {
    match profile_hint::hint(entry, hosts, &ops.saved_profiles()) {
        Some(h) => (format!("{text}\n{}", h.text), h.chip),
        None => (text, None),
    }
}

/// Profile- and socket-backed [`AllowOps`].
pub(super) struct ProfileAllowOps<'a> {
    pub agent: &'a str,
    pub home: std::path::PathBuf,
}

impl AllowOps for ProfileAllowOps<'_> {
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>> {
        Ok(crate::cmd::agent::load_profile_for_edit(self.agent)?
            .1
            .mcp_servers)
    }
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<()> {
        let (path, mut p) = crate::cmd::agent::load_profile_for_edit(self.agent)?;
        p.mcp_servers = servers;
        crate::cmd::agent::save_profile(&path, &mut p)
    }
    fn grant_live(&mut self, host: &str, session: bool) -> Result<(), String> {
        let mut params = serde_json::json!({
            "server": LIVE_ENTRY,
            "host": host,
            "session": session,
            // `attach` adds the approval token only to an allow.
            "allow": true,
        });
        mur_common::hitl::approval_token::attach(&mut params, &self.home)
            .map_err(|e| format!("could not read the approval token: {e}"))?;
        crate::a2a_dial::dial_method(
            &self.home,
            self.agent,
            mur_common::egress_event::GRANT_METHOD,
            params,
            crate::a2a_dial::DialMode::RequireRunning,
        )
        .map(|_| ())
        .map_err(|e| format!("{e:#}"))
    }
    fn saved_profiles(&mut self) -> Vec<SavedProfile> {
        profile_hint::load_saved_profiles(&self.home)
    }
}

/// `/browser status`: the same table as `mur agent network`, plus which
/// saved login the `browser` entry launches with (#2100).
pub(super) fn run_status(agent: &str, home: &std::path::Path) -> anyhow::Result<Managed> {
    let (_, p) = crate::cmd::agent::load_profile_for_edit(agent)?;
    let table = crate::cmd::agent::network_view::render(&p);
    Ok(status_tail(
        table,
        &p.mcp_servers,
        &profile_hint::load_saved_profiles(home),
    ))
}

/// The profile line and hint under the network table; the table alone when
/// the agent has no `browser` entry (nothing launches, nothing to attach).
fn status_tail(table: String, servers: &[McpServerEntry], saved: &[SavedProfile]) -> Managed {
    let Some(entry) = servers.iter().find(|e| e.name == LIVE_ENTRY) else {
        return (table, None);
    };
    let text = format!("{table}\n{}", profile_hint::status_line(entry));
    let hosts = entry
        .network
        .as_ref()
        .map(|n| n.allow_hosts.clone())
        .unwrap_or_default();
    match profile_hint::hint(entry, &hosts, saved) {
        Some(h) => (format!("{text}\n{}", h.text), h.chip),
        None => (text, None),
    }
}

#[cfg(test)]
#[path = "browser_allow_cmd_tests.rs"]
mod tests;
