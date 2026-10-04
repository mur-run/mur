//! Task 3.6 (pure half): print the plan, compare it with the setup
//! manifest, and decide what needs consent.
//!
//! The manifest records what the user already agreed to for one agent, so
//! a re-run asks only about the difference. It lives under `<mur_home>`,
//! outside every agent's write grant: an agent that could edit it could
//! mark `--lsp rust` as approved and skip the prompt.
//!
//! Re-runs never remove anything. A language dropped from the flags stays
//! granted until the user revokes it (`mur agent perm deny-spawn`); setup
//! only says so.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::plan::{LspStatus, Permission, Plan, Tier};
use super::serena_install::Record;

/// `<mur_home>/<MANIFEST_DIR>/<agent>.json`.
pub const MANIFEST_DIR: [&str; 2] = ["setup", "code-nav"];
const MANIFEST_EXT: &str = "json";

/// What a setup run applied, and what it could not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// `name@version` per installed tool.
    pub installed: BTreeSet<String>,
    /// Grant strings, see [`grant_key`].
    pub granted: BTreeSet<String>,
    /// `--lsp` values written to `project.yml`.
    pub languages: BTreeSet<String>,
    /// Project serena serves, if serena was set up.
    pub project: Option<PathBuf>,
    pub serena: Option<Record>,
    /// The pyright serena launches via `ls_path` (3.6b). Absent in
    /// manifests written before it existed.
    #[serde(default)]
    pub pyright: Option<super::pyright_install::Record>,
    /// Finding 4: every disabled line, so "off" is never silent.
    pub disabled: Vec<String>,
}

pub fn manifest_path(mur_home: &Path, agent: &str) -> PathBuf {
    let mut p = mur_home.to_path_buf();
    p.extend(MANIFEST_DIR);
    p.join(agent).with_extension(MANIFEST_EXT)
}

/// Missing file = nothing consented yet. A corrupt one is an error, never
/// silently "nothing", so a damaged manifest cannot widen anything.
pub fn load_manifest(path: &Path) -> Result<Manifest> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub fn save_manifest(path: &Path, m: &Manifest) -> Result<()> {
    let dir = path.parent().context("manifest path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let tmp = path.with_extension("json.mur-tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(m)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))
}

pub fn install_key(name: &str, version: &str) -> String {
    format!("{name}@{version}")
}

pub fn grant_key(p: &Permission) -> String {
    match p {
        Permission::Spawn(b) => format!("spawn {b}"),
        Permission::Read(d) => format!("read {}", d.display()),
        Permission::SpawnDir(d) => format!("spawn-dir {}", d.display()),
    }
}

/// What this plan would add on top of the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub installs: Vec<String>,
    pub grants: Vec<String>,
    pub languages: Vec<String>,
    /// A different project than last time.
    pub project: Option<PathBuf>,
    /// Consented before, not in this plan. Reported, never revoked.
    pub kept_languages: Vec<String>,
}

impl Diff {
    /// Nothing new to consent to.
    pub fn is_empty(&self) -> bool {
        self.installs.is_empty()
            && self.grants.is_empty()
            && self.languages.is_empty()
            && self.project.is_none()
    }
}

pub fn enabled_languages(plan: &Plan) -> Vec<String> {
    plan.lsp
        .iter()
        .filter(|r| r.status == LspStatus::Enabled)
        .map(|r| r.lang.flag().to_owned())
        .collect()
}

pub fn diff(plan: &Plan, project: Option<&Path>, m: &Manifest) -> Diff {
    let installs = plan
        .install
        .iter()
        .filter(|r| r.missing.is_none())
        .map(|r| install_key(r.name, r.version))
        .filter(|k| !m.installed.contains(k))
        .collect();
    let grants = plan
        .permissions
        .iter()
        .map(grant_key)
        .filter(|k| !m.granted.contains(k))
        .collect();
    let enabled = enabled_languages(plan);
    let languages = enabled
        .iter()
        .filter(|l| !m.languages.contains(*l))
        .cloned()
        .collect();
    let kept_languages = m
        .languages
        .iter()
        .filter(|l| !enabled.contains(l))
        .cloned()
        .collect();
    let project = project
        .filter(|p| m.project.as_deref() != Some(*p))
        .map(Path::to_path_buf);
    Diff {
        installs,
        grants,
        languages,
        project,
        kept_languages,
    }
}

fn tier_label(t: Tier) -> &'static str {
    match t {
        Tier::Low => "low",
        Tier::MediumContained => "medium (contained)",
        Tier::High => "HIGH",
    }
}

/// The three tables (install, permissions, LSP risk), then the disabled
/// lines (finding 4). Skipped languages show the flag that enables them.
pub fn print_tables(out: &mut dyn Write, plan: &Plan) -> Result<()> {
    writeln!(out, "Install:")?;
    for r in &plan.install {
        match r.missing {
            None => writeln!(
                out,
                "    {:<10} {:<12} {}",
                r.name,
                r.version,
                r.dir.display()
            )?,
            Some(m) => writeln!(
                out,
                "    {:<10} {:<12} disabled: {m} not found on PATH",
                r.name, r.version
            )?,
        }
    }
    if plan.install.is_empty() {
        writeln!(out, "    (nothing)")?;
    }
    if !plan.permissions.is_empty() {
        writeln!(out, "\nPermissions for the agent:")?;
        for p in &plan.permissions {
            writeln!(out, "    {}", grant_key(p))?;
        }
    }
    if !plan.lsp.is_empty() {
        writeln!(out, "\nLanguage servers (serena):")?;
        for r in &plan.lsp {
            let status = match &r.status {
                LspStatus::Enabled => "enabled".to_owned(),
                LspStatus::Skipped { enable_flag } => format!("skipped ({enable_flag})"),
                LspStatus::Disabled { missing } => format!("disabled: {missing} not found on PATH"),
            };
            writeln!(
                out,
                "    {:<11} {:<19} {status}",
                r.lang.flag(),
                tier_label(r.tier)
            )?;
            if let Some(note) = r.note.filter(|_| r.status == LspStatus::Enabled) {
                writeln!(out, "    {:<11} note: {note}", "")?;
            }
        }
    }
    for line in plan.disabled_lines() {
        writeln!(out, "  ! {line}")?;
    }
    Ok(())
}

/// What a re-run is about to add, so the prompt names only the difference.
pub fn print_diff(out: &mut dyn Write, d: &Diff, first_run: bool) -> Result<()> {
    if !d.kept_languages.is_empty() {
        writeln!(
            out,
            "\nStill enabled from an earlier run (setup never revokes): {}",
            d.kept_languages.join(", ")
        )?;
    }
    if first_run || d.is_empty() {
        return Ok(());
    }
    writeln!(out, "\nNew since the last setup for this agent:")?;
    for i in &d.installs {
        writeln!(out, "    install   {i}")?;
    }
    for g in &d.grants {
        writeln!(out, "    grant     {g}")?;
    }
    for l in &d.languages {
        writeln!(out, "    language  {l}")?;
    }
    if let Some(p) = &d.project {
        writeln!(out, "    project   {}", p.display())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "consent_tests.rs"]
mod tests;
