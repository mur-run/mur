//! Which projects each agent's serena entries serve, and which languages
//! each project gets (#2016 Decisions 1 and 3).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::super::plan::Lang;
use super::super::serena_entry;
use super::detect::detect_langs;
use super::names;
use super::prompt::Prompt;
use super::{AgentCtx, Project};

/// The last item of the shared picker.
pub(super) const ADD_PATH: &str = "Add a path…";

/// Refuse a manual project that would hand serena (and the read grant) far
/// too much: the filesystem root, the home directory itself, or anything
/// under MUR's own home. `home` and `mur_home` are canonical.
pub(super) fn checked_path(raw: &Path, home: Option<&Path>, mur_home: &Path) -> Result<PathBuf> {
    let p = serena_entry::checked_project(raw)?;
    if p.parent().is_none() {
        bail!("{}: refusing the filesystem root as a project", p.display());
    }
    if home == Some(p.as_path()) {
        bail!(
            "{}: refusing your home directory itself as a project; pick the repository",
            p.display()
        );
    }
    if p.starts_with(mur_home) {
        bail!(
            "{}: refusing a path inside MUR's own home ({})",
            p.display(),
            mur_home.display()
        );
    }
    Ok(p)
}

/// `(name, path)` rows from `mur project list` → usable projects, saying
/// why each skipped row was skipped. Deduplicated by canonical path.
pub(super) fn listed(rows: Vec<(String, String)>, out: &mut dyn Write) -> Result<Vec<Project>> {
    let mut v: Vec<Project> = Vec::new();
    for (name, path) in rows {
        if path.is_empty() {
            writeln!(out, "  skip {name}: the index records no project path")?;
            continue;
        }
        let Ok(canon) = std::fs::canonicalize(&path) else {
            writeln!(out, "  skip {name}: {path} no longer exists")?;
            continue;
        };
        if !canon.is_dir() {
            writeln!(out, "  skip {name}: {path} is not a directory")?;
            continue;
        }
        if v.iter().all(|p| p.path != canon) {
            v.push(Project::new(name, canon, true));
        }
    }
    Ok(v)
}

/// Add `path` unless a project with the same canonical path is listed.
/// Returns its index either way.
pub(super) fn add_project(projects: &mut Vec<Project>, path: PathBuf, indexed: bool) -> usize {
    if let Some(i) = projects.iter().position(|p| p.path == path) {
        return i;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    projects.push(Project::new(name, path, indexed));
    projects.len() - 1
}

/// Projects already served by some named agent but not in `mur project
/// list` join the picker too, so they can be preselected.
pub(super) fn add_served(projects: &mut Vec<Project>, agents: &[AgentCtx]) {
    for a in agents {
        for (_, p) in &a.existing.known {
            if p.is_dir() {
                add_project(projects, p.clone(), false);
            }
        }
    }
}

/// Projects every agent already serves: the preselection (Decision 1), and
/// the whole selection under `--yes`.
pub(super) fn served_by_all(projects: &[Project], agents: &[AgentCtx]) -> BTreeSet<usize> {
    (0..projects.len())
        .filter(|i| agents.iter().all(|a| a.existing.serves(&projects[*i].path)))
        .collect()
}

/// `served by pm (qa not selected)` for a project only some agents serve.
pub(super) fn partly_served(p: &Project, agents: &[AgentCtx]) -> Option<String> {
    let (by, not): (Vec<&AgentCtx>, Vec<&AgentCtx>) =
        agents.iter().partition(|a| a.existing.serves(&p.path));
    if by.is_empty() || not.is_empty() {
        return None;
    }
    let names = |v: &[&AgentCtx]| {
        v.iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(format!(
        "served by {} ({} not selected)",
        names(&by),
        names(&not)
    ))
}

fn label(p: &Project, agents: &[AgentCtx]) -> String {
    let mut s = format!("{}  {}", p.name, p.path.display());
    if !p.indexed {
        s.push_str("  [not indexed]");
    }
    if let Some(note) = partly_served(p, agents) {
        s.push_str(&format!("  [{note}]"));
    }
    s
}

/// Decision 1 step one, plus `Add a path…`: one multi-select for every
/// agent. Returns the ticked projects.
pub(super) fn shared(
    projects: &mut Vec<Project>,
    agents: &[AgentCtx],
    home: Option<&Path>,
    mur_home: &Path,
    preset: &BTreeSet<usize>,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<BTreeSet<usize>> {
    let mut ticked = preset.clone();
    loop {
        let mut items: Vec<String> = projects.iter().map(|p| label(p, agents)).collect();
        items.push(ADD_PATH.to_owned());
        let defaults: Vec<bool> = (0..items.len()).map(|i| ticked.contains(&i)).collect();
        let picked = prompt.multi_select(
            "Projects serena should serve for every agent (space to tick, enter to confirm; none = ast-grep only)",
            &items,
            &defaults,
        )?;
        let add = picked.contains(&projects.len());
        ticked = picked.into_iter().filter(|i| *i < projects.len()).collect();
        if !add {
            return Ok(ticked);
        }
        let raw = prompt.input("Project directory", "")?;
        if raw.trim().is_empty() {
            continue;
        }
        let raw = PathBuf::from(shellexpand_tilde(raw.trim(), home));
        match checked_path(&raw, home, mur_home) {
            Ok(p) => {
                let i = add_project(projects, p, false);
                ticked.insert(i);
            }
            Err(e) => writeln!(out, "  ! {e:#}")?,
        }
    }
}

fn shellexpand_tilde(raw: &str, home: Option<&Path>) -> String {
    match (raw.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => h.join(rest).display().to_string(),
        _ if raw == "~" => home.map(|h| h.display().to_string()).unwrap_or_default(),
        _ => raw.to_owned(),
    }
}

/// Decision 1 step two: on `y`, one multi-select per agent, defaulting to
/// the shared set. Returns each agent's projects, in `agents` order.
pub(super) fn per_agent(
    projects: &[Project],
    agents: &[AgentCtx],
    shared: &BTreeSet<usize>,
    prompt: &mut dyn Prompt,
) -> Result<Vec<BTreeSet<usize>>> {
    if agents.len() < 2 || !prompt.confirm("Customize per agent?")? {
        return Ok(vec![shared.clone(); agents.len()]);
    }
    let items: Vec<String> = projects.iter().map(|p| label(p, agents)).collect();
    let defaults: Vec<bool> = (0..items.len()).map(|i| shared.contains(&i)).collect();
    agents
        .iter()
        .map(|a| {
            let v = prompt.multi_select(&format!("Projects for {}", a.name), &items, &defaults)?;
            Ok(v.into_iter().collect())
        })
        .collect()
}

/// Languages for one project, asked at most once (Decision: per project,
/// not per agent). Detected languages that need no opt-in start ticked;
/// Lua, C/C++ and the High tier start unticked. `ask == false` (`--yes`)
/// keeps exactly the ticked defaults.
pub(super) fn languages(p: &Project, ask: bool, prompt: &mut dyn Prompt) -> Result<BTreeSet<Lang>> {
    let detected: Vec<Lang> = p.detected.iter().copied().collect();
    let defaults: BTreeSet<Lang> = detected
        .iter()
        .copied()
        .filter(|l| l.on_by_default())
        .collect();
    if !ask || detected.is_empty() {
        return Ok(defaults);
    }
    let items: Vec<String> = detected
        .iter()
        .map(|l| {
            let tier = super::super::consent::tier_label(l.tier());
            let opt = if l.on_by_default() { "" } else { "  (opt-in)" };
            format!("{:<11} {tier}{opt}", l.flag())
        })
        .collect();
    let ticks: Vec<bool> = detected.iter().map(|l| defaults.contains(l)).collect();
    let picked = prompt.multi_select(
        &format!("Language servers for {} ({})", p.name, p.path.display()),
        &items,
        &ticks,
    )?;
    Ok(picked.into_iter().map(|i| detected[i]).collect())
}

impl Project {
    pub(super) fn new(name: String, path: PathBuf, indexed: bool) -> Self {
        let detected = detect_langs(&path);
        let base = names::sanitize(&name);
        Self {
            name,
            path,
            indexed,
            detected,
            base,
        }
    }
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
