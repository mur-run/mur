//! Bind a freshly saved browser profile to an agent's live `browser` entry,
//! right after `mur browser auth` (#2199).
//!
//! Only [`BindPlan::Bind`] ever writes, and it only rewrites the entry's
//! `--profile` argv — never its network policy. A profile is **added
//! alongside** the ones already bound when their domains are disjoint
//! (#2263); `replace` drops them instead, and an overlap without `replace` is
//! refused. Every other plan is refused with the reason, and for `NeedsHosts`
//! the full `/browser live` command to run instead. Writes go through [`LiveOps::save_if_unchanged`], which
//! shares `/browser live`'s save path but refuses if the entries changed
//! since the user reviewed them; each write appends a `profile_bound` trace
//! event.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::Result;
use mur_common::agent::{McpNetMode, McpServerEntry};

use super::bind_plan::BoundDomains;
use super::bind_plan::{BindPlan, plan_bind, warning};
use super::live_entry::{LIVE_ENTRY, LiveOps, attached_profiles, set_profiles};

/// One agent offered in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub agent: String,
    pub current: Vec<String>,
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
    bound: &BoundDomains<'_>,
) -> Vec<Candidate> {
    agents
        .into_iter()
        .filter_map(|(agent, servers)| {
            let entry = servers.into_iter().find(|e| e.name == LIVE_ENTRY)?;
            Some(Candidate {
                current: attached_profiles(&entry)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                plan: plan_bind(Some(&entry), site, domains, bound, false),
                agent,
            })
        })
        .collect()
}

/// `none`, or the bound profiles joined for a status row.
pub(crate) fn current_label(current: &[String]) -> String {
    if current.is_empty() {
        "none".to_owned()
    } else {
        current.join(", ")
    }
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
        let current = current_label(&c.current);
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
/// `/browser live` can never turn a refusal into a write. `bound` resolves
/// the domains of profiles already on the entry; `replace` drops them instead
/// of adding `site` alongside. `confirm` is the interactive reader; `None`
/// writes without asking (the `--bind` flag is the consent).
#[allow(clippy::too_many_arguments)] // one call site per flow; the args are the plan's inputs
pub(crate) fn bind_agent(
    agent: &str,
    site: &str,
    domains: &[String],
    bound: &BoundDomains<'_>,
    replace: bool,
    ops: &mut dyn LiveOps,
    confirm: Option<&mut dyn BufRead>,
    out: &mut dyn Write,
) -> Result<Outcome> {
    let mut servers = ops.servers()?;
    let reviewed = servers.clone();
    let entry = servers.iter().find(|e| e.name == LIVE_ENTRY);
    let before: Vec<String> = entry
        .map(|e| {
            attached_profiles(e)
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    // Already carrying `site`: with `replace` the others still go; without
    // it there is nothing to change.
    if before.iter().any(|p| p == site) && (!replace || before.len() == 1) {
        writeln!(
            out,
            "{agent} already launches with {site:?}; nothing to bind."
        )?;
        return Ok(Outcome::Unchanged);
    }
    let plan = plan_bind(entry, site, domains, bound, replace);
    let BindPlan::Bind {
        alongside,
        replaces,
    } = plan
    else {
        writeln!(out, "{}", refusal(agent, site, &plan))?;
        return Ok(Outcome::Refused);
    };
    let undo = undo_line(agent, entry.expect("Bind implies an entry"), &before);
    let entry = servers
        .iter_mut()
        .find(|e| e.name == LIVE_ENTRY)
        .expect("Bind implies an entry");
    let mut after = alongside.clone();
    after.push(site.to_owned());
    set_profiles(entry, &after);
    // The trace's `old` is what this write dropped, if anything.
    let replaced = (!replaces.is_empty()).then(|| replaces.join(","));

    let repairs = ops.pending_repairs(&servers)?;
    if let Some(input) = confirm {
        writeln!(out, "{}", intent_line(agent, site, &alongside, &replaces))?;
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

    // Compare-and-write: the confirmation can sit open for minutes, and
    // saving `servers` blindly would put back a host or entry another
    // terminal removed meanwhile — widening egress the user never approved.
    let Some((notes, effect)) = ops
        .save_if_unchanged(&reviewed, servers)
        .map_err(|e| e.context("could not save the browser profile binding"))?
    else {
        writeln!(
            out,
            "✗ {agent}'s MCP servers changed while this was waiting — not bound. \
             Run `mur browser auth {site}` again to review the current entry."
        )?;
        return Ok(Outcome::Refused);
    };
    for note in &notes {
        writeln!(out, "{note}")?;
    }
    writeln!(out, "{}", done_line(agent, site, &alongside, &replaces))?;
    writeln!(out, "{}", effect.applied_line())?;
    // Telemetry must not undo a write that already succeeded.
    if let Err(e) = ops.record_profile_bound(agent, replaced.as_deref(), site) {
        writeln!(
            out,
            "could not record the profile-binding audit event: {e:#}"
        )?;
    }
    writeln!(out, "{undo}")?;
    Ok(Outcome::Bound)
}

fn quoted(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("{n:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The confirmation prompt's first line: what the write will do.
fn intent_line(agent: &str, site: &str, alongside: &[String], replaces: &[String]) -> String {
    if !replaces.is_empty() {
        format!("bind {site:?} to {agent}, replacing {}", quoted(replaces))
    } else if !alongside.is_empty() {
        format!("bind {site:?} to {agent} alongside {}", quoted(alongside))
    } else {
        format!("bind {site:?} to {agent}")
    }
}

/// What the write did, after it succeeded.
fn done_line(agent: &str, site: &str, alongside: &[String], replaces: &[String]) -> String {
    if !replaces.is_empty() {
        format!("bound {site:?} to {agent} (was {})", quoted(replaces))
    } else if !alongside.is_empty() {
        format!("bound {site:?} to {agent} alongside {}", quoted(alongside))
    } else {
        format!("bound {site:?} to {agent}")
    }
}

/// How to put the old binding back: a `/browser live` rerun with the same
/// scope that re-points the entry at exactly the profiles it had, or drops
/// them all (`--no-profile`) when there were none.
fn undo_line(agent: &str, entry: &McpServerEntry, before: &[String]) -> String {
    let profile = if before.is_empty() {
        "--no-profile".to_owned()
    } else {
        before
            .iter()
            .map(|old| format!("--profile {old}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let scope = match entry.network.as_ref().map(|n| n.mode) {
        Some(McpNetMode::BroadAudited) => "--research".to_owned(),
        _ => entry
            .network
            .as_ref()
            .map(|n| n.allow_hosts.join(" "))
            .unwrap_or_default(),
    };
    format!("undo, inside murmur in the {agent} conversation:\n  /browser live {scope} {profile}")
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
        BindPlan::Overlaps { with, .. } => {
            // Adding would merge two logins for one host; replacing is a
            // choice the user makes explicitly, with the whole profile list.
            format!(
                "✗ {agent}: {why} — not bound. One live session cannot hold two logins \
                 for the same host. To use {site:?} instead of {with:?}, run inside \
                 murmur, in the {agent} conversation:\n  \
                 /browser live <hosts> --profile {site}\n  \
                 or rerun `mur browser auth {site} --bind {agent} --replace`."
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
    replace: bool,
    out: &mut dyn Write,
) -> Result<()> {
    let domains = saved_domains(home, site)?;
    let bound = bound_domains(home);
    let mut ops = crate::cmd::agent::cli::browser_live_cmd::ProfileOps(agent);
    let outcome = bind_agent(agent, site, &domains, &bound, replace, &mut ops, None, out)?;
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
/// 10), or an error before any login starts: a typo, or case variants with no
/// exact match, which would otherwise bind whichever agent `read_dir` lists
/// first. Same selection as `a2a_dial::canonicalize_agent_name`, but it fails
/// instead of passing the input through.
pub(crate) fn resolve_agent(home: &std::path::Path, typed: &str) -> Result<String> {
    use crate::a2a_dial::{NameMatch, agent_dir_names, match_agent_name};
    match match_agent_name(&agent_dir_names(home), typed) {
        NameMatch::Found(name) => Ok(name),
        NameMatch::Missing => {
            anyhow::bail!("--bind {typed:?}: no such agent (see `mur agent list`)")
        }
        NameMatch::Ambiguous(mut names) => {
            names.sort();
            anyhow::bail!(
                "--bind {typed:?} is ambiguous: agents {} differ only by case; \
                 type the exact name",
                names.join(", ")
            )
        }
    }
}

fn saved_domains(home: &std::path::Path, site: &str) -> Result<Vec<String>> {
    let meta = super::read_profile_meta(&mur_browser::paths::profile_meta(home, site))?;
    super::bind_plan::profile_domains(&meta)
        .ok_or_else(|| anyhow::anyhow!("the profile names no host"))
}

/// The planner's lookup for profiles already bound to an entry: their saved
/// domains, or `None` when the metadata is missing or unreadable.
pub(crate) fn bound_domains(home: &std::path::Path) -> impl Fn(&str) -> Option<Vec<String>> {
    let home = home.to_path_buf();
    move |site: &str| saved_domains(&home, site).ok()
}

fn offer(home: &std::path::Path, site: &str, tty: bool, out: &mut dyn Write) -> Result<()> {
    let domains = saved_domains(home, site)?;
    let bound = bound_domains(home);
    let cands = candidates(live_agents(home), site, &domains, &bound);
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
    // The menu is the add path; an overlap tells the user how to replace.
    bind_agent(
        agent,
        site,
        &domains,
        &bound,
        false,
        &mut ops,
        Some(&mut input),
        out,
    )?;
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
