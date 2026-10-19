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
//!
//! The exec lanes for serena's and pyright's Python are only knowable after
//! install, so the plan cannot list them. `apply` diffs them against the
//! manifest with [`new_lanes`] and asks again before granting any it has
//! not recorded (#1942).
//!
//! `--yes` is a convenience for scripted runs, not a guard against an agent:
//! anything that can run `mur code-nav setup --yes` can grant itself the
//! plan. The protection is that the manifest and the agent's profile sit
//! outside every agent's write grant, not the confirmation prompt.

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
    /// Project per additional (non-default) serena entry, by entry name.
    /// Absent in manifests written before multi-entry setup.
    #[serde(default)]
    pub entry_projects: std::collections::BTreeMap<String, PathBuf>,
    /// `--lsp` values written to each entry's `project.yml`, by entry name
    /// (the default entry included). `languages` stays the agent-wide
    /// consent history; this is what each project actually lists (#1943).
    /// Absent in manifests written before per-entry languages: such an
    /// entry's next setup lists only the languages enabled for it.
    #[serde(default)]
    pub entry_languages: std::collections::BTreeMap<String, BTreeSet<String>>,
    pub serena: Option<Record>,
    /// The pyright serena launches via `ls_path` (3.6b). Absent in
    /// manifests written before it existed.
    #[serde(default)]
    pub pyright: Option<super::pyright_install::Record>,
    /// The intelephense serena launches via the canonical `ls_base_cmd`
    /// (#1945). Absent in manifests written before it existed.
    #[serde(default)]
    pub intelephense: Option<super::node_ls_install::Record>,
    /// The typescript-language-server serena launches via the canonical
    /// `ls_base_cmd` (#1981). Absent in manifests written before it existed.
    #[serde(default)]
    pub typescript: Option<super::node_ls_install::Record>,
    /// The Vue server serena finds through its linked resources dir
    /// (#1982). Absent in manifests written before it existed.
    #[serde(default)]
    pub vue: Option<super::node_ls_install::Record>,
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
        Permission::SpawnDir(d) => lane_grant_key(d),
        Permission::ReadWrite(d) => format!("read-write {}", d.display()),
        Permission::Mcp { name, command } => format!("mcp {name} {}", command.display()),
    }
}

/// The manifest key an exec lane is recorded under.
pub fn lane_grant_key(dir: &Path) -> String {
    format!("spawn-dir {}", dir.display())
}

/// #1942: lanes resolved after install that the manifest never recorded.
pub fn new_lanes(lanes: &[PathBuf], m: &Manifest) -> Vec<PathBuf> {
    lanes
        .iter()
        .filter(|l| !m.granted.contains(&lane_grant_key(l)))
        .cloned()
        .collect()
}

/// What this plan would add on top of the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub installs: Vec<String>,
    pub grants: Vec<String>,
    pub languages: Vec<String>,
    /// A different project than last time.
    pub project: Option<PathBuf>,
    /// Listed for this entry before, not in this plan. Reported, never revoked.
    pub kept_languages: Vec<String>,
    /// `--lsp-only` (#1992): listed for this entry before, removed by this
    /// run. Narrowing needs no consent, but it is never silent.
    pub dropped_languages: Vec<String>,
    /// Consented for the agent (another entry, or a manifest older than
    /// per-entry languages) but not listed for this entry. Reported so the
    /// narrower list is never silent.
    pub other_languages: Vec<String>,
    /// #1942 note 1: the project this one replaces, whose read grant stays.
    pub kept_project: Option<PathBuf>,
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

impl Manifest {
    /// The project last applied for `entry`.
    pub fn project_of(&self, entry: &str) -> Option<&Path> {
        if entry == mur_agent_runtime::mcp::serena::DEFAULT_ENTRY_NAME {
            self.project.as_deref()
        } else {
            self.entry_projects.get(entry).map(PathBuf::as_path)
        }
    }

    /// The languages last written to `entry`'s `project.yml`, if recorded.
    pub fn languages_of(&self, entry: &str) -> Option<&BTreeSet<String>> {
        self.entry_languages.get(entry)
    }

    /// Record the languages written for `entry`. The agent-wide set keeps
    /// them too, as consent history.
    pub fn set_languages(&mut self, entry: &str, langs: impl IntoIterator<Item = String>) {
        let langs: BTreeSet<String> = langs.into_iter().collect();
        self.languages.extend(langs.iter().cloned());
        self.entry_languages.insert(entry.to_owned(), langs);
    }

    /// Record the project applied for `entry`.
    pub fn set_project(&mut self, entry: &str, project: &Path) {
        if entry == mur_agent_runtime::mcp::serena::DEFAULT_ENTRY_NAME {
            self.project = Some(project.to_path_buf());
        } else {
            self.entry_projects
                .insert(entry.to_owned(), project.to_path_buf());
        }
    }
}

pub fn diff(plan: &Plan, entry: &str, project: Option<&Path>, m: &Manifest) -> Diff {
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
    let empty = BTreeSet::new();
    let own = m.languages_of(entry).unwrap_or(&empty);
    let (dropped_languages, kept_languages): (Vec<String>, Vec<String>) = own
        .iter()
        .filter(|l| !enabled.contains(l))
        .cloned()
        .partition(|_| plan.exact_lsp);
    let other_languages = m
        .languages
        .iter()
        .filter(|l| !enabled.contains(l) && !own.contains(*l))
        .cloned()
        .collect();
    let project = project
        .filter(|p| m.project_of(entry) != Some(*p))
        .map(Path::to_path_buf);
    let kept_project = project
        .as_ref()
        .and(m.project_of(entry))
        .map(Path::to_path_buf);
    Diff {
        installs,
        grants,
        languages,
        project,
        kept_languages,
        dropped_languages,
        other_languages,
        kept_project,
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
    if !d.dropped_languages.is_empty() {
        writeln!(
            out,
            "\nDropped from this entry (--lsp-only): {}",
            d.dropped_languages.join(", ")
        )?;
    }
    if !d.kept_languages.is_empty() {
        writeln!(
            out,
            "\nStill enabled from an earlier run (setup never revokes): {}",
            d.kept_languages.join(", ")
        )?;
    }
    if !d.other_languages.is_empty() {
        writeln!(
            out,
            "\nConsented for this agent but not listed for this entry: {} \
             (pass --lsp to add)",
            d.other_languages.join(", ")
        )?;
    }
    if let Some(p) = &d.kept_project {
        writeln!(
            out,
            "\nStill granted from the replaced project (setup never revokes): read {0} \
             (revoke: mur agent perm remove-path <agent> read {0})",
            p.display()
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
