//! `mur agent skill` — list / add / remove / show skills attached to an agent.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use mur_common::AgentProfile as _AgentProfile;

use super::{load_profile_for_edit, resolve_mur_home, save_profile};

/// Resolve a user-supplied skill query against a profile's skill list.
/// Tries exact match, then trailing path component, then trailing path component
/// without the `.md` suffix. Returns the canonical stored id on success.
fn resolve_skill_id<'a>(profile: &'a _AgentProfile, query: &str) -> Option<&'a String> {
    if let Some(s) = profile.skills.iter().find(|s| s.as_str() == query) {
        return Some(s);
    }
    if let Some(s) = profile.skills.iter().find(|s| {
        Path::new(s.as_str())
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f == query)
    }) {
        return Some(s);
    }
    if let Some(s) = profile.skills.iter().find(|s| {
        Path::new(s.as_str())
            .file_stem()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f == query)
    }) {
        return Some(s);
    }
    None
}

pub fn cmd_skill_list(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    if profile.skills.is_empty() {
        println!("(no skills attached)");
        return Ok(());
    }
    for s in &profile.skills {
        println!("{s}");
    }
    Ok(())
}

pub fn cmd_skill_add(name: &str, source: &str) -> Result<()> {
    let src = PathBuf::from(source);
    if !src.exists() {
        bail!("skill source '{source}' not found");
    }
    let basename = src
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("skill source has no basename"))?;
    let (path, mut profile) = load_profile_for_edit(name)?;

    let agent_home = path.parent().unwrap_or(Path::new(""));
    let skills_dir = agent_home.join("skills");
    fs::create_dir_all(&skills_dir).with_context(|| format!("create {}", skills_dir.display()))?;
    let dest = skills_dir.join(basename);
    fs::copy(&src, &dest)
        .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;

    let skill_id = format!("skills/{basename}");
    if !profile.skills.iter().any(|s| s == &skill_id) {
        profile.skills.push(skill_id);
    }
    save_profile(&path, &mut profile)
}

pub fn cmd_skill_remove(name: &str, query: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let resolved = resolve_skill_id(&profile, query)
        .ok_or_else(|| anyhow!("skill '{query}' not found on '{name}'"))?
        .clone();
    profile.skills.retain(|s| s != &resolved);
    save_profile(&path, &mut profile)?;

    // Delete the backing file if no other skill entry references it.
    let agent_home = resolve_mur_home()?.join("agents").join(name);
    let file_path = agent_home.join(&resolved);
    if file_path.exists() && !profile.skills.iter().any(|s| s == &resolved) {
        let _ = fs::remove_file(&file_path);
    }
    Ok(())
}

pub fn cmd_skill_show(name: &str, query: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let resolved = resolve_skill_id(&profile, query)
        .ok_or_else(|| anyhow!("skill '{query}' not registered on '{name}'"))?;
    let agent_home = resolve_mur_home()?.join("agents").join(name);
    let file_path = agent_home.join(resolved);
    let body =
        fs::read_to_string(&file_path).with_context(|| format!("read {}", file_path.display()))?;
    print!("{body}");
    Ok(())
}
