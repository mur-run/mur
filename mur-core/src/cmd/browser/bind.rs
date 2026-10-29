//! Bind a freshly saved browser profile to an agent's live `browser` entry,
//! right after `mur browser auth` (#2199).
//!
//! Only [`BindPlan::Bind`] ever writes, and it only rewrites the entry's
//! `--profile` argv — never its network policy. Every other plan is refused
//! with the reason, and for `NeedsHosts` the full `/browser live` command to
//! run instead. Writes go through [`LiveOps::save`], the same path as
//! `/browser live`, and each one appends a `profile_bound` trace event.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::Result;
use mur_common::agent::{McpNetMode, McpServerEntry};

use super::bind_plan::{BindPlan, plan_bind, warning};
use super::live_entry::{LIVE_ENTRY, LiveOps, attached_profile, set_profile};

/// One agent offered in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub agent: String,
    pub current: Option<String>,
    pub plan: BindPlan,
}

/// What [`bind_agent`] did. `--bind` maps everything but `Bound` and
/// `Unchanged` to a non-zero exit ([`flag_result`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Bound,
    Unchanged,
    Declined,
    Refused,
}

/// Agents that have a live entry, each with the plan for binding `site`.
pub(crate) fn candidates(
    agents: Vec<(String, Vec<McpServerEntry>)>,
    site: &str,
    domains: &[String],
) -> Vec<Candidate> {
    agents
        .into_iter()
        .filter_map(|(agent, servers)| {
            let entry = servers.into_iter().find(|e| e.name == LIVE_ENTRY)?;
            Some(Candidate {
                current: attached_profile(&entry).map(str::to_owned),
                plan: plan_bind(Some(&entry), site, domains),
                agent,
            })
        })
        .collect()
}

/// Print the current bindings and ask which agent gets `site`. Enter, EOF, or
/// no candidates means skip; nothing is ever chosen by default.
pub(crate) fn menu(
    site: &str,
    cands: &[Candidate],
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Option<usize>> {
    if cands.is_empty() {
        writeln!(out, "{}", no_agents_hint(site))?;
        return Ok(None);
    }
    writeln!(out, "current bindings:")?;
    let w = cands.iter().map(|c| c.agent.len()).max().unwrap_or(0);
    for (i, c) in cands.iter().enumerate() {
        let warn = warning(&c.plan)
            .map(|w| format!("  ⚠ {w}"))
            .unwrap_or_default();
        let current = c.current.as_deref().unwrap_or("none");
        writeln!(out, "  [{}] {:w$}  → {current}{warn}", i + 1, c.agent)?;
    }
    loop {
        write!(
            out,
            "bind {site:?} to which agent? [1-{}, Enter = skip] ",
            cands.len()
        )?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(out)?;
            return Ok(None);
        }
        match line.trim() {
            "" => return Ok(None),
            s => match s.parse::<usize>() {
                Ok(n) if (1..=cands.len()).contains(&n) => return Ok(Some(n - 1)),
                _ => writeln!(out, "not a choice: {s:?}")?,
            },
        }
    }
}

/// Bind `site` to `agent`'s live entry. The plan is recomputed from what
/// `ops` reads now, not from the menu's snapshot, so a concurrent
/// `/browser live` can never turn a refusal into a write. `confirm` is the
/// interactive reader; `None` writes without asking (the `--bind` flag is the
/// consent).
pub(crate) fn bind_agent(
    agent: &str,
    site: &str,
    domains: &[String],
    ops: &mut dyn LiveOps,
    confirm: Option<&mut dyn BufRead>,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let mut servers = ops.servers()?;
    let entry = servers.iter().find(|e| e.name == LIVE_ENTRY);
    if entry.and_then(attached_profile) == Some(site) {
        writeln!(
            out,
            "{agent} already launches with {site:?}; nothing to bind."
        )?;
        return Ok(Outcome::Unchanged);
    }
    let plan = plan_bind(entry, site, domains);
    let BindPlan::Bind { replaces } = plan else {
        writeln!(out, "{}", refusal(agent, site, &plan))?;
        return Ok(Outcome::Refused);
    };
    let undo = undo_line(
        agent,
        entry.expect("Bind implies an entry"),
        replaces.as_deref(),
    );
    let entry = servers
        .iter_mut()
        .find(|e| e.name == LIVE_ENTRY)
        .expect("Bind implies an entry");
    set_profile(entry, site);

    let repairs = ops.pending_repairs(&servers)?;
    if let Some(input) = confirm {
        match &replaces {
            Some(old) => writeln!(out, "bind {site:?} to {agent}, replacing {old:?}")?,
            None => writeln!(out, "bind {site:?} to {agent}")?,
        }
        if !repairs.is_empty() {
            writeln!(out, "also repairs:\n  {}", repairs.join("\n  "))?;
        }
        write!(out, "proceed? [y/N] ")?;
        out.flush()?;
        let mut line = String::new();
        input.read_line(&mut line)?;
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            writeln!(out, "not bound.")?;
            return Ok(Outcome::Declined);
        }
    }

    let (notes, effect) = ops
        .save(servers)
        .map_err(|e| e.context("could not save the browser profile binding"))?;
    for note in &notes {
        writeln!(out, "{note}")?;
    }
    match &replaces {
        Some(old) => writeln!(out, "bound {site:?} to {agent} (was {old:?})")?,
        None => writeln!(out, "bound {site:?} to {agent}")?,
    }
    writeln!(out, "{}", effect.applied_line())?;
    // Telemetry must not undo a write that already succeeded.
    if let Err(e) = ops.record_profile_bound(agent, replaces.as_deref(), site) {
        writeln!(
            out,
            "could not record the profile-binding audit event: {e:#}"
        )?;
    }
    writeln!(out, "{undo}")?;
    Ok(Outcome::Bound)
}

/// How to put the old binding back. A `/browser live` rerun that names a
/// profile re-points the entry, so a replaced profile has a one-command undo;
/// a first-time bind has none today, and says so rather than print a command
/// that would keep the new profile.
fn undo_line(agent: &str, entry: &McpServerEntry, replaces: Option<&str>) -> String {
    let Some(old) = replaces else {
        return format!(
            "undo: there is no one-command unbind yet; {agent}'s live entry keeps this \
             profile until another one is bound."
        );
    };
    let scope = match entry.network.as_ref().map(|n| n.mode) {
        Some(McpNetMode::BroadAudited) => "--research".to_owned(),
        _ => entry
            .network
            .as_ref()
            .map(|n| n.allow_hosts.join(" "))
            .unwrap_or_default(),
    };
    format!(
        "undo, inside murmur in the {agent} conversation:\n  /browser live {scope} --profile {old}"
    )
}

/// Why a non-`Bind` plan was refused, plus the command to run when one exists.
fn refusal(agent: &str, site: &str, plan: &BindPlan) -> String {
    let why = warning(plan).unwrap_or_default();
    match plan {
        BindPlan::NeedsHosts { current, missing } => {
            // `/browser live` replaces the host list: print all of it.
            let mut hosts = current.clone();
            hosts.extend(missing.iter().filter(|m| !current.contains(m)).cloned());
            format!(
                "✗ {agent}'s live entry does not allow {} — not bound.\n  \
                 Run inside murmur, in the {agent} conversation:\n  \
                 /browser live {} --profile {site}",
                missing.join(", "),
                hosts.join(" ")
            )
        }
        BindPlan::Denied { .. } => {
            format!("✗ {agent}: {why} — not bound. Removing a deny entry is an operator decision.")
        }
        BindPlan::NotHostScoped { .. } => {
            format!("✗ {agent}: {why} — not bound. A profile is only bound to a host-scoped entry.")
        }
        BindPlan::NoLiveEntry => format!(
            "✗ {agent} has no live browser entry — not bound.\n  \
             Create one inside murmur, in the {agent} conversation:\n  \
             /browser live <hosts> --profile {site}"
        ),
        BindPlan::Bind { .. } => unreachable!("Bind is not a refusal"),
    }
}

/// After `mur browser auth` saved `site`: offer the binding menu on a TTY;
/// otherwise list the bindings and the hint, ask nothing, write nothing (the
/// `Consent::Impossible` rule of `setup.rs`). Best-effort: the profile is
/// already saved, so a failure here is reported, never propagated.
pub(crate) fn after_auth(home: &std::path::Path, site: &str, out: &mut dyn Write) {
    if let Err(e) = offer(home, site, std::io::stdin().is_terminal(), out) {
        let _ = writeln!(out, "could not offer a binding for {site:?}: {e:#}");
    }
}

/// `--bind <agent>`: the flag is the consent, so bind without asking. Any
/// refusal is a non-zero exit; the profile itself is already saved.
pub(crate) fn bind_flag(
    home: &std::path::Path,
    site: &str,
    agent: &str,
    out: &mut dyn Write,
) -> Result<()> {
    let domains = saved_domains(home, site)?;
    let mut ops = crate::cmd::agent::cli::browser_live_cmd::ProfileOps(agent);
    let outcome = bind_agent(agent, site, &domains, &mut ops, None, out)?;
    out.flush()?;
    flag_result(&outcome, agent, site)
}

/// Exit status for `--bind`: only a write, or a binding already in place, is
/// success.
fn flag_result(outcome: &Outcome, agent: &str, site: &str) -> Result<()> {
    match outcome {
        Outcome::Bound | Outcome::Unchanged => Ok(()),
        Outcome::Declined | Outcome::Refused => {
            anyhow::bail!("profile {site:?} was saved but not bound to {agent} (see above)")
        }
    }
}

/// The on-disk name of an existing agent (case-insensitive, CLAUDE.md rule
/// 10), or an error naming the typo before any login starts. Matched against
/// the directory listing rather than a path probe: on a case-insensitive
/// volume `agents/Reviewer/profile.yaml` exists for `reviewer`, and the
/// trace must carry the real name.
pub(crate) fn resolve_agent(home: &std::path::Path, typed: &str) -> Result<String> {
    let names: Vec<String> = std::fs::read_dir(home.join("agents"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("profile.yaml").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names
        .iter()
        .find(|n| *n == typed)
        .or_else(|| names.iter().find(|n| n.eq_ignore_ascii_case(typed)))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("--bind {typed:?}: no such agent (see `mur agent list`)"))
}

fn saved_domains(home: &std::path::Path, site: &str) -> Result<Vec<String>> {
    let meta = super::read_profile_meta(&mur_browser::paths::profile_meta(home, site))?;
    super::bind_plan::profile_domains(&meta)
        .ok_or_else(|| anyhow::anyhow!("the profile names no host"))
}

fn offer(home: &std::path::Path, site: &str, tty: bool, out: &mut dyn Write) -> Result<()> {
    let domains = saved_domains(home, site)?;
    let cands = candidates(live_agents(home), site, &domains);
    if !tty {
        if cands.is_empty() {
            writeln!(out, "{}", no_agents_hint(site))?;
        } else {
            for line in super::bindings::section(home) {
                writeln!(out, "{line}")?;
            }
            writeln!(
                out,
                "not bound (no terminal). Bind it inside murmur with \
                 `/browser live <hosts> --profile {site}`."
            )?;
        }
        return Ok(());
    }
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let Some(i) = menu(site, &cands, &mut input, out)? else {
        return Ok(());
    };
    let agent = &cands[i].agent;
    let mut ops = crate::cmd::agent::cli::browser_live_cmd::ProfileOps(agent);
    bind_agent(agent, site, &domains, &mut ops, Some(&mut input), out)?;
    Ok(())
}

/// Every agent's MCP servers, sorted by name. An unreadable profile is
/// skipped here; `mur browser status` is where it gets reported.
fn live_agents(home: &std::path::Path) -> Vec<(String, Vec<McpServerEntry>)> {
    let Ok(dir) = std::fs::read_dir(home.join("agents")) else {
        return Vec::new();
    };
    let mut agents: Vec<(String, Vec<McpServerEntry>)> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let yaml = std::fs::read_to_string(e.path().join("profile.yaml")).ok()?;
            let p: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml).ok()?;
            Some((name, p.mcp_servers))
        })
        .collect();
    agents.sort_by(|a, b| a.0.cmp(&b.0));
    agents
}

fn no_agents_hint(site: &str) -> String {
    format!(
        "no agent has a live browser entry yet. To use {site:?}, run inside murmur:\n  \
         /browser live <hosts> --profile {site}"
    )
}

#[cfg(test)]
#[path = "bind_tests.rs"]
mod tests;
