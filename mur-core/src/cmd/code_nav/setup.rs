//! Task 3.6 (impure half): `mur code-nav setup`. Detect, plan, print, ask
//! once, apply, record.
//!
//! Consent follows `mur browser setup` (`cmd::consent`): a literal `yes` on a
//! terminal, or `--yes`. `--yes` confirms the printed plan; flags alone
//! decide its scope. No terminal and no `--yes` refuses before anything is
//! installed, written or granted. A re-run whose plan adds nothing over the
//! manifest asks nothing and only re-verifies.
//!
//! The exec lanes are the one thing the plan cannot list: after install,
//! any lane the manifest has not recorded is printed by path and asked
//! about again, under the same rules, before it is granted (#1942). A run
//! that fails part way saves no manifest, so the next run re-lists what it
//! had already applied: conservative, never silent.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use mur_agent_runtime::mcp::serena::serena_paths_for;

use super::consent::{self, Manifest};
use super::plan::{self, Detected, Flags, Lang, LspStatus, Permission, UV};
use super::{ast_grep_wire, serena_entry, smoke, vue_install};
use crate::cmd::browser::setup::Consent;

mod apply;
mod lanes;
mod smoke_run;

#[cfg(test)]
use apply::entry_langs;
use apply::{Target, apply};
use lanes::Ask;
#[cfg(test)]
use lanes::consent_to_lanes;
#[cfg(test)]
use serde_yaml_ng::Value;
use smoke_run::smoke_targets;

pub struct Args {
    pub agent: String,
    pub project: Option<PathBuf>,
    /// Name of the `kind: serena` entry this run creates or updates.
    pub entry: String,
    pub flags: Flags,
}

/// `PATH` lookup without a `which` dependency.
fn on_path(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(format!("{tool}{}", std::env::consts::EXE_SUFFIX)))
        .find(|c| c.is_file())
}

fn detect() -> Detected {
    let mut tools: BTreeSet<&str> = Lang::ALL
        .iter()
        .flat_map(|l| l.prerequisites().iter().copied())
        .collect();
    tools.insert(UV);
    Detected {
        on_path: tools
            .into_iter()
            .filter(|t| on_path(t).is_some())
            .map(str::to_owned)
            .collect(),
    }
}

pub fn run(
    args: Args,
    consent: Consent,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let agent = crate::a2a_dial::canonicalize_agent_name(&mur_home, &args.agent);
    let agent_home = mur_home.join("agents").join(&agent);
    if !agent_home.join("profile.yaml").is_file() {
        bail!("agent '{agent}' not found");
    }
    // Refuse a name serena_paths_for would refuse at spawn, before anything
    // is installed: the home is derived from the name alone.
    serena_paths_for(&agent_home, &args.entry)?;
    let entry = args.entry.as_str();
    let project = match (&args.project, args.flags.with_serena) {
        (Some(p), true) => Some(serena_entry::checked_project(p)?),
        (None, true) => bail!("--with-serena needs --project <dir>: the repo serena will serve"),
        (Some(_), false) => bail!("--project only applies with --with-serena"),
        (None, false) => None,
    };
    // A same-named server the user owns would only be refused at the final
    // profile save, after uv downloads and config writes. Refuse it here,
    // before the plan is even printed, so nothing changes on disk.
    if project.is_some() {
        let (_, profile) = crate::cmd::agent::load_profile_for_edit(&agent)?;
        serena_entry::check_slot(&profile, entry)?;
    }

    let plan = plan::plan(&mur_home, &args.flags, &detect())?;
    let mut permissions = plan.permissions.clone();
    // #1984: what wiring ast-grep to the agent grants, decided (and any
    // refusal raised) before anything is installed.
    if plan.install.iter().any(|r| r.name == plan::AST_GREP) {
        let slot = ast_grep_wire::preflight(&agent)?;
        permissions.extend(ast_grep_wire::permissions(&mur_home, &slot));
    }
    if let Some(p) = &project
        && plan
            .install
            .iter()
            .any(|r| r.name == plan::SERENA && r.missing.is_none())
    {
        permissions.push(Permission::Read(p.clone()));
    }
    let plan = plan::Plan {
        permissions,
        ..plan
    };

    writeln!(out, "mur code-nav setup for agent '{agent}'\n")?;
    consent::print_tables(out, &plan)?;
    if let Some(hint) = vue_hint(project.as_deref(), &plan) {
        writeln!(out, "\n{hint}")?;
    }
    if let Some(p) = &project {
        writeln!(out, "\nserena entry `{entry}` project: {}", p.display())?;
        writeln!(
            out,
            "    also needed after install: exec of serena's (and pyright's) own Python \
             (each venv bin dir and the interpreter uv chose); any not granted before \
             is shown by path and asked about then"
        )?;
    }

    let manifest_path = consent::manifest_path(&mur_home, &agent);
    let prior = consent::load_manifest(&manifest_path)?;
    let first_run = prior == Manifest::default();
    let diff = consent::diff(&plan, entry, project.as_deref(), &prior);
    consent::print_diff(out, &diff, first_run)?;

    if diff.is_empty() {
        writeln!(
            out,
            "\nNothing new to consent to; re-verifying what is installed."
        )?;
    } else {
        match consent {
            Consent::Impossible => bail!(
                "`mur code-nav setup` needs a terminal to ask for consent. Pass --yes to \
                 confirm the plan above (flags decide what it contains)"
            ),
            Consent::Ask if !crate::cmd::consent::literal_yes(input, out)? => {
                writeln!(
                    out,
                    "  skipped — nothing was installed, written or granted."
                )?;
                bail!("code-nav setup not applied");
            }
            _ => {}
        }
    }

    let manifest = apply(
        &Target {
            mur_home: &mur_home,
            agent: &agent,
            agent_home: &agent_home,
            entry,
        },
        &plan,
        project.as_deref(),
        prior,
        &mut Ask { consent, input },
        out,
    )?;
    consent::save_manifest(&manifest_path, &manifest)?;
    writeln!(out, "\nRecorded in {}", manifest_path.display())?;

    // #1981 Layer 3: success means the servers start under the agent's seal,
    // not that the files landed. Run after the manifest is saved, so a
    // failure here keeps the installs and grants recorded above.
    if let Some(project) = &project
        && manifest.serena.is_some()
    {
        let langs: Vec<Lang> = Lang::ALL
            .into_iter()
            .filter(|l| {
                manifest
                    .languages_of(entry)
                    .is_some_and(|s| s.contains(l.flag()))
            })
            .collect();
        let serena_home = serena_paths_for(&agent_home, entry)?.home;
        let (targets, unfetched) = smoke_targets(&manifest, &langs, &mur_home, &serena_home, out)?;
        let fetch_dir = smoke::fetch_dir(&serena_home);
        smoke::verify(&agent, project, &targets, &unfetched, &fetch_dir, out)?;
    }
    Ok(())
}

/// #1982: a project with `.vue` files but Vue off gets told how to turn it
/// on. Vue is High tier, so detection never enables it by itself.
pub(super) fn vue_hint(project: Option<&Path>, plan: &plan::Plan) -> Option<String> {
    let row = plan.lsp.iter().find(|r| r.lang == Lang::Vue)?;
    let LspStatus::Skipped { enable_flag } = &row.status else {
        return None;
    };
    vue_install::project_has_vue(project?).then(|| {
        format!(
            "  hint: this project has .{} files; re-run with `{enable_flag}` for symbol \
             navigation in them (implies --lsp typescript)",
            vue_install::VUE_EXT
        )
    })
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod run_tests;
