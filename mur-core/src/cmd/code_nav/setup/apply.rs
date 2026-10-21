//! The apply step of `mur code-nav setup`, in three phases (#2016):
//!
//! 1. [`prepare`] — pure: what each step will do, from the plan and the
//!    prior manifest.
//! 2. [`install`](super::install::install) — downloads and installs, writes
//!    serena's files, and resolves the exec lanes.
//! 3. [`commit`] — the serena profile entry and grants via `save_profile`,
//!    then the manifest.
//!
//! The lane question sits between install and commit, so a caller with
//! several targets can install them all, ask once, then commit each.
//! ast-grep is wired to the agent inside install, right after its download,
//! and install saves the manifest at that point (#2018): those grants are
//! in the profile from then on, so a later failure must not lose their
//! record. Nothing else is recorded before commit.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use super::super::ast_grep_install;
use super::super::consent::{self, Manifest};
use super::super::plan::{self, Lang, LspStatus, Permission};
use super::super::serena_entry;
use super::install::{Installed, install_with};
use super::lanes::{Ask, consent_to_lanes};

/// Whose profile and which serena entry a run applies to.
pub(super) struct Target<'a> {
    pub(super) mur_home: &'a Path,
    pub(super) agent: &'a str,
    pub(super) agent_home: &'a Path,
    pub(super) entry: &'a str,
    /// The agent's manifest: saved once ast-grep is wired (#2018) and at
    /// the end of [`commit`].
    pub(super) manifest_path: &'a Path,
}

/// What [`install`] will do for one target. Built by [`prepare`].
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Prepared<'p> {
    /// The prior manifest, with this plan's disabled lines.
    pub(super) manifest: Manifest,
    /// The ast-grep version to install and wire, if the plan has it.
    pub(super) ast_grep: Option<&'static str>,
    pub(super) serena: SerenaStep<'p>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SerenaStep<'p> {
    /// No `--with-serena`: ast-grep only.
    Off,
    /// The plan has serena but uv is not on PATH: say so, set nothing up.
    NoUv,
    Install(SerenaPlan<'p>),
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SerenaPlan<'p> {
    pub(super) project: &'p Path,
    pub(super) pyright: bool,
    pub(super) intelephense: bool,
    pub(super) typescript: bool,
    pub(super) vue: bool,
    /// What this entry's `project.yml` will list ([`entry_langs`]).
    pub(super) langs: Vec<Lang>,
}

/// Phase 1. Pure: reads only `plan` and `prior`.
pub(super) fn prepare<'p>(
    entry: &str,
    plan: &plan::Plan,
    project: Option<&'p Path>,
    prior: Manifest,
) -> Prepared<'p> {
    let has = |name: &str| plan.install.iter().any(|r| r.name == name);
    let ast_grep = plan
        .install
        .iter()
        .find(|r| r.name == plan::AST_GREP && r.missing.is_none())
        .map(|r| r.version);
    let serena = match project {
        None => SerenaStep::Off,
        Some(_)
            if plan
                .install
                .iter()
                .any(|r| r.name == plan::SERENA && r.missing.is_some()) =>
        {
            SerenaStep::NoUv
        }
        Some(project) => SerenaStep::Install(SerenaPlan {
            project,
            pyright: has(plan::PYRIGHT),
            intelephense: has(mur_common::serena_ls_cmd::INTELEPHENSE.tool_dir),
            typescript: has(mur_common::serena_ls_cmd::TYPESCRIPT.tool_dir),
            vue: has(mur_common::serena_ls_cmd::VUE.tool_dir),
            langs: entry_langs(&prior, entry, plan),
        }),
    };
    let mut manifest = prior;
    manifest.disabled = plan.disabled_lines();
    Prepared {
        manifest,
        ast_grep,
        serena,
    }
}

/// One target, all three phases, asking about new exec lanes in between.
/// A failure in any phase returns before the final manifest save; only the
/// ast-grep step, if it was reached, is already recorded (#2018).
pub(super) fn apply(
    t: &Target<'_>,
    plan: &plan::Plan,
    project: Option<&Path>,
    prior: Manifest,
    ask: &mut Ask<'_>,
    out: &mut dyn Write,
) -> Result<Manifest> {
    apply_with(
        t,
        plan,
        project,
        prior,
        ask,
        out,
        &ast_grep_install::install,
    )
}

/// [`apply`] with the ast-grep installer injected, so a test can reach the
/// steps after it without a download.
pub(super) fn apply_with(
    t: &Target<'_>,
    plan: &plan::Plan,
    project: Option<&Path>,
    prior: Manifest,
    ask: &mut Ask<'_>,
    out: &mut dyn Write,
    install_ast_grep: &dyn Fn(&Path) -> Result<ast_grep_install::Outcome>,
) -> Result<Manifest> {
    let prepared = prepare(t.entry, plan, project, prior);
    let installed = install_with(t, prepared, out, install_ast_grep)?;
    consent_to_lanes(
        installed.lanes(),
        &installed.manifest,
        ask.consent,
        ask.input,
        out,
    )?;
    commit(t, plan, installed, out)
}

/// Phase 3: the serena profile entry, its grants and exec lanes, then the
/// manifest. Call only once the lanes in `installed` are consented to.
pub(super) fn commit(
    t: &Target<'_>,
    plan: &plan::Plan,
    installed: Installed<'_>,
    out: &mut dyn Write,
) -> Result<Manifest> {
    let Target {
        agent,
        entry,
        manifest_path,
        ..
    } = *t;
    let Installed {
        manifest: mut m,
        serena,
    } = installed;
    if let Some(s) = serena {
        let (ppath, mut profile) = crate::cmd::agent::load_profile_for_edit(agent)?;
        let change = serena_entry::upsert(
            &mut profile,
            serena_entry::build_named(&s.record, entry, s.project)?,
        )?;
        crate::cmd::agent::save_profile(&ppath, &mut profile)?;
        writeln!(out, "  ✓ profile entry `{entry}`: {change:?}")?;

        // The quiet `grant_*` forms: the consent table above already named
        // each grant, and the running-agent warning is printed once, below,
        // instead of once per grant.
        for p in &plan.permissions {
            match p {
                Permission::Spawn(b) => crate::cmd::agent::grant_spawn(agent, b)?,
                Permission::Read(d) => crate::cmd::agent::grant_read(agent, path_str(d)?)?,
                Permission::SpawnDir(d) => crate::cmd::agent::grant_spawn_dir(agent, path_str(d)?)?,
                // Applied by `ast_grep_wire` right after the ast-grep install.
                Permission::ReadWrite(_) | Permission::Mcp { .. } => continue,
            }
            m.granted.insert(consent::grant_key(p));
        }
        for lane in &s.lanes {
            crate::cmd::agent::grant_spawn_dir(agent, path_str(lane)?)?;
            m.granted.insert(consent::lane_grant_key(lane));
            writeln!(out, "  ✓ exec lane {}", lane.display())?;
        }
        writeln!(out, "  ✓ permissions granted")?;
        crate::cmd::agent::warn_if_running(agent);

        m.set_languages(entry, s.langs.iter().map(|l| l.flag().to_owned()));
        m.set_project(entry, s.project);
        m.serena = Some(s.record);
        m.pyright = s.pyright;
        m.intelephense = s.intelephense;
        m.typescript = s.typescript;
        m.vue = s.vue;
    }
    consent::save_manifest(manifest_path, &m)?;
    Ok(m)
}

/// The languages `entry`'s `project.yml` lists: what this plan enables,
/// unioned with what this entry listed before (setup never revokes), or,
/// with `--lsp-only` (#1992), exactly what this plan enables. Only
/// this entry's: serena refuses to start if any listed server fails, so
/// another project's languages must not leak in (#1943).
pub(super) fn entry_langs(m: &Manifest, entry: &str, plan: &plan::Plan) -> Vec<Lang> {
    let own = m.languages_of(entry).filter(|_| !plan.exact_lsp);
    Lang::ALL
        .into_iter()
        .filter(|l| {
            own.is_some_and(|o| o.contains(l.flag()))
                || plan
                    .lsp
                    .iter()
                    .any(|r| r.lang == *l && r.status == LspStatus::Enabled)
        })
        .collect()
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .with_context(|| format!("path {} is not UTF-8", p.display()))
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
