//! `mur code-nav setup --wizard <agent>...` (#2016): several agents, several
//! projects, one plan. Drives the same `plan` / `consent` / prepare →
//! install → commit code as single-agent setup; it never grows `setup::run`.
//!
//! Flow: pick projects (shared, then optionally per agent) → name each
//! (agent, project) entry, reusing by path → pick languages per project →
//! print one combined plan → one literal `yes` → shared installs once →
//! install every target → one merged question for new exec lanes → commit
//! each target → smoke → offer to restart the agents that changed.
//!
//! `--yes` confirms the printed plan and nothing more: the selection is
//! fixed (projects every agent already serves, plus `--path`), no opt-in
//! language is enabled, and agents are restarted only with `--restart`.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::consent::{self, Diff, Manifest};
use super::plan::{self, Flags, Lang};
use crate::cmd::browser::setup::Consent;

mod build;
mod detect;
mod exec;
mod names;
mod ops;
mod prompt;
mod select;
mod table;

use names::{Existing, Naming};
use prompt::{Prompt, Terminal};

pub struct Args {
    /// Agent names as typed (case-insensitive).
    pub agents: Vec<String>,
    /// `--path <dir>`, repeatable: projects added without the picker.
    pub paths: Vec<PathBuf>,
    /// `--restart`: restart changed agents without asking (step 2).
    pub restart: bool,
}

/// A project the wizard can offer.
#[derive(Debug, Clone)]
pub(in crate::cmd::code_nav::wizard) struct Project {
    /// From `mur project list`, or the directory name for `--path`.
    name: String,
    /// Canonical.
    path: PathBuf,
    /// In `mur project list`. An unindexed path still works for serena.
    indexed: bool,
    detected: BTreeSet<Lang>,
    /// [`names::sanitize`]d `name`: the entry name it would get.
    base: String,
}

/// One named agent, read once before anything is printed.
#[derive(Debug)]
pub(in crate::cmd::code_nav::wizard) struct AgentCtx {
    /// Canonical name.
    name: String,
    home: PathBuf,
    existing: Existing,
    manifest_path: PathBuf,
    prior: Manifest,
}

/// One (agent, entry) the wizard applies: the unit of every report row.
#[derive(Debug)]
pub(in crate::cmd::code_nav::wizard) struct Spec {
    agent: usize,
    /// Index into the projects; `None` is ast-grep only.
    project: Option<usize>,
    /// Set with a project.
    naming: Option<Naming>,
    flags: Flags,
    plan: plan::Plan,
    diff: Diff,
    /// Why this target is not applied before anything runs: skipped by the
    /// user, or not decidable under `--yes`.
    hold: Option<Hold>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::cmd::code_nav::wizard) enum Hold {
    /// The user chose to skip it, or there is nothing to serve: not a failure.
    Skipped(String),
    /// Cannot be applied as printed — `--yes` cannot answer a question it
    /// needs, or its plan was refused: a failure.
    Blocked(String),
}

impl Spec {
    fn entry(&self) -> &str {
        self.naming.as_ref().map_or(
            mur_agent_runtime::mcp::serena::DEFAULT_ENTRY_NAME,
            Naming::entry,
        )
    }

    /// Something to install, grant or write, and nothing holding it back.
    fn applies(&self) -> bool {
        self.hold.is_none() && !self.diff.is_empty()
    }
}

/// Everything the wizard decided, ready to print and apply.
#[derive(Debug)]
pub(in crate::cmd::code_nav::wizard) struct Decided {
    agents: Vec<AgentCtx>,
    projects: Vec<Project>,
    specs: Vec<Spec>,
    /// Projects an agent serves that were left unticked: never removed.
    kept: Vec<(usize, String, PathBuf)>,
    /// Duplicate entries on one path (reused the first; warned).
    duplicates: Vec<(usize, String, Vec<String>)>,
}

pub fn run(
    args: Args,
    consent: Consent,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    if consent == Consent::Impossible {
        bail!(
            "`mur code-nav setup --wizard` asks questions and there is no terminal. Run it in \
             a terminal, pass --yes to apply the fixed selection (projects every agent already \
             serves, plus --path), or use explicit `mur code-nav setup --agent …` flags"
        );
    }
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let agents = load_agents(&mur_home, &args.agents)?;
    let mut terminal = Terminal { input };
    let decided = decide(&mur_home, agents, &args.paths, consent, &mut terminal, out)?;
    let mut ops = ops::Real::new(&mur_home, &decided);
    exec::apply(
        &decided,
        consent,
        args.restart,
        &mut terminal,
        &mut ops,
        out,
    )
}

/// Canonical, deduplicated agents. Any unknown name fails here, before a
/// single line is printed.
fn load_agents(mur_home: &Path, raw: &[String]) -> Result<Vec<AgentCtx>> {
    if raw.is_empty() {
        bail!("--wizard needs at least one agent name");
    }
    let mut names: Vec<String> = Vec::new();
    for r in raw {
        let n = crate::a2a_dial::canonicalize_agent_name(mur_home, r);
        if !mur_home
            .join("agents")
            .join(&n)
            .join("profile.yaml")
            .is_file()
        {
            bail!("agent '{r}' not found");
        }
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let (_, profile) = crate::cmd::agent::load_profile_for_edit(&name)?;
            let manifest_path = consent::manifest_path(mur_home, &name);
            let prior = consent::load_manifest(&manifest_path)?;
            Ok(AgentCtx {
                home: mur_home.join("agents").join(&name),
                existing: Existing::read(&profile, &prior),
                name,
                manifest_path,
                prior,
            })
        })
        .collect()
}

/// Pick, name and plan. Writes nothing; Esc or a refusal returns an error.
fn decide(
    mur_home: &Path,
    agents: Vec<AgentCtx>,
    paths: &[PathBuf],
    consent: Consent,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<Decided> {
    let home = dirs::home_dir().and_then(|h| std::fs::canonicalize(h).ok());
    let mur_canon = std::fs::canonicalize(mur_home).unwrap_or_else(|_| mur_home.to_path_buf());
    writeln!(
        out,
        "mur code-nav setup --wizard for {}\n",
        agents
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    let rows = crate::cmd::project::do_project_list()?
        .into_iter()
        .map(|p| (p.name, p.path))
        .collect();
    let mut projects = select::listed(rows, out)?;
    select::add_served(&mut projects, &agents);
    let mut added = BTreeSet::new();
    for p in paths {
        let p = select::checked_path(p, home.as_deref(), &mur_canon)?;
        added.insert(select::add_project(&mut projects, p, false));
    }
    let preset: BTreeSet<usize> = select::served_by_all(&projects, &agents)
        .union(&added)
        .copied()
        .collect();
    let ask = consent == Consent::Ask;
    let per_agent = if ask {
        let shared = select::shared(
            &mut projects,
            &agents,
            home.as_deref(),
            &mur_canon,
            &preset,
            prompt,
            out,
        )?;
        select::per_agent(&projects, &agents, &shared, prompt)?
    } else {
        vec![preset; agents.len()]
    };
    build::build(mur_home, agents, projects, per_agent, ask, prompt, out)
}

/// POSIX-shell quoting for the equivalent commands the plan prints.
fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+~@:=".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;
#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
