//! The side effects the wizard's apply makes, behind one trait so the flow in
//! [`super::exec`] (shared installs once, merged lane question, per-target
//! commit, restart eligibility, exit code) is testable without a network.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::super::consent::{self, Manifest};
use super::super::plan::{self, InstallRow, UV};
use super::super::setup::apply::{Target, commit, prepare};
use super::super::setup::install::{Installed, install};
use super::super::setup::{on_path, smoke_entry};
use super::super::{ast_grep_install, node_ls_install, pyright_install, serena_install};
use super::Decided;

pub(super) trait Ops {
    /// What [`Ops::install`] hands to [`Ops::commit`].
    type Staged;
    /// One tool under `<mur_home>/tools`, once for every target.
    fn install_shared(&mut self, row: &InstallRow, out: &mut dyn Write) -> Result<()>;
    /// Spec `i`'s install phase; also returns the exec lanes not yet
    /// consented to.
    fn install(&mut self, i: usize, out: &mut dyn Write) -> Result<(Self::Staged, Vec<PathBuf>)>;
    /// Spec `i`'s commit phase: profile entry, grants, lanes, manifest.
    fn commit(&mut self, i: usize, staged: Self::Staged, out: &mut dyn Write) -> Result<Manifest>;
    /// Spec `i`'s smoke launch, after its commit.
    fn smoke(&mut self, i: usize, m: &Manifest, out: &mut dyn Write) -> Result<()>;
    fn running(&self, agent: &str) -> bool;
    /// `Ok((ok, detail))` from `restart_quiet`.
    fn restart(&mut self, agent: &str) -> Result<(bool, String)>;
}

pub(super) struct Real<'a> {
    mur_home: &'a Path,
    d: &'a Decided,
}

impl<'a> Real<'a> {
    pub(super) fn new(mur_home: &'a Path, d: &'a Decided) -> Self {
        Self { mur_home, d }
    }

    fn target(&self, i: usize) -> Target<'a> {
        let s = &self.d.specs[i];
        let a = &self.d.agents[s.agent];
        Target {
            mur_home: self.mur_home,
            agent: &a.name,
            agent_home: &a.home,
            entry: s.entry(),
            manifest_path: &a.manifest_path,
            warn_running: false,
        }
    }

    fn project(&self, i: usize) -> Option<&'a Path> {
        self.d.specs[i]
            .project
            .map(|p| self.d.projects[p].path.as_path())
    }
}

impl<'a> Ops for Real<'a> {
    type Staged = Installed<'a>;

    fn install_shared(&mut self, row: &InstallRow, out: &mut dyn Write) -> Result<()> {
        let home = self.mur_home;
        let uv = || on_path(UV).with_context(|| format!("{UV} not found on PATH"));
        let label = format!("{} {}", row.name, row.version);
        match row.name {
            plan::AST_GREP => {
                let dest = mur_common::config::ast_grep_binary_path(home);
                let o = ast_grep_install::install(&dest)?;
                writeln!(out, "  ✓ {label} ({o:?})")?;
            }
            plan::SERENA => {
                let (o, _) =
                    serena_install::install_with(&uv()?, &serena_install::serena_dir(home))?;
                writeln!(out, "  ✓ {label} ({o:?})")?;
            }
            plan::PYRIGHT => {
                let (o, _) =
                    pyright_install::install_with(&uv()?, &pyright_install::pyright_dir(home))?;
                writeln!(out, "  ✓ {label} ({o:?})")?;
            }
            name => {
                use mur_common::serena_ls_cmd::{INTELEPHENSE, TYPESCRIPT, VUE};
                let ls = [INTELEPHENSE, TYPESCRIPT, VUE]
                    .into_iter()
                    .find(|l| l.tool_dir == name)
                    .with_context(|| format!("no shared installer for {name}"))?;
                let npm = on_path(plan::NPM)
                    .with_context(|| format!("{} not found on PATH", plan::NPM))?;
                let (o, _) =
                    node_ls_install::install_with(&ls, &npm, &node_ls_install::tools_dir(home))?;
                writeln!(out, "  ✓ {label} ({o:?})")?;
            }
        }
        Ok(())
    }

    fn install(&mut self, i: usize, out: &mut dyn Write) -> Result<(Installed<'a>, Vec<PathBuf>)> {
        let s = &self.d.specs[i];
        let a = &self.d.agents[s.agent];
        // Re-read: an earlier target of this agent may have committed.
        let prior = consent::load_manifest(&a.manifest_path)?;
        let t = self.target(i);
        let installed = install(&t, prepare(t.entry, &s.plan, self.project(i), prior), out)?;
        let lanes = consent::new_lanes(installed.lanes(), &installed.manifest);
        Ok((installed, lanes))
    }

    fn commit(
        &mut self,
        i: usize,
        mut staged: Installed<'a>,
        out: &mut dyn Write,
    ) -> Result<Manifest> {
        let s = &self.d.specs[i];
        // Every install ran before any commit, so `staged` lacks what this
        // agent's earlier targets committed since; fold it back in.
        let on_disk = consent::load_manifest(&self.d.agents[s.agent].manifest_path)?;
        staged.manifest = merged(on_disk, staged.manifest);
        commit(&self.target(i), &s.plan, staged, out)
    }

    fn smoke(&mut self, i: usize, m: &Manifest, out: &mut dyn Write) -> Result<()> {
        let Some(project) = self.project(i) else {
            return Ok(());
        };
        if m.serena.is_none() {
            return Ok(());
        }
        let t = self.target(i);
        smoke_entry(
            self.mur_home,
            t.agent,
            t.agent_home,
            t.entry,
            project,
            m,
            out,
        )
    }

    fn running(&self, agent: &str) -> bool {
        let lock = self
            .mur_home
            .join("agents")
            .join(agent)
            .join("running.lock");
        mur_common::lock_file::classify(&lock).kind
            == mur_common::lock_file::AgentStatusKind::Running
    }

    fn restart(&mut self, agent: &str) -> Result<(bool, String)> {
        let r = crate::cmd::agent::restart_quiet(agent)?;
        Ok((r.ok, r.detail))
    }
}

/// `staged` (read before this run's earlier commits, plus its own installs)
/// merged onto `on_disk` (those commits). Sets and per-entry maps union; a
/// tool record this run produced wins over the stored one.
pub(super) fn merged(mut on_disk: Manifest, staged: Manifest) -> Manifest {
    on_disk.installed.extend(staged.installed);
    on_disk.granted.extend(staged.granted);
    on_disk.languages.extend(staged.languages);
    for (k, v) in staged.entry_projects {
        on_disk.entry_projects.entry(k).or_insert(v);
    }
    for (k, v) in staged.entry_languages {
        on_disk.entry_languages.entry(k).or_insert(v);
    }
    on_disk.project = on_disk.project.or(staged.project);
    on_disk.serena = staged.serena.or(on_disk.serena);
    on_disk.pyright = staged.pyright.or(on_disk.pyright);
    on_disk.intelephense = staged.intelephense.or(on_disk.intelephense);
    on_disk.typescript = staged.typescript.or(on_disk.typescript);
    on_disk.vue = staged.vue.or(on_disk.vue);
    on_disk.disabled = staged.disabled;
    on_disk
}
