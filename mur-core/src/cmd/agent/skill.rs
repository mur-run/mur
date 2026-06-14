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

    let text = fs::read_to_string(&src)
        .with_context(|| format!("read skill source '{}'", src.display()))?;

    // Parse the input into a canonical manifest regardless of extension:
    // `.yaml`/`.yml` via the canonical parser, anything else (`.md`, no ext)
    // via the markdown-frontmatter parser. A source that does not parse into a
    // manifest is rejected — we never copy a dead file that the loader would
    // silently ignore.
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let manifest = if ext == "yaml" || ext == "yml" {
        mur_common::skill::parse_canonical(&text)
            .map_err(|e| anyhow!("not a valid skill manifest: {e}. See `mur skill new`."))?
    } else {
        mur_common::skill::parse_markdown(&text)
            .map_err(|e| anyhow!("not a valid skill manifest: {e}. See `mur skill new`."))?
    };

    // Schema validation + security content scan for ALL inputs.
    mur_common::skill::validate(&manifest).map_err(|e| anyhow!("skill validation failed: {e}"))?;
    let report =
        mur_common::skill::scan::scan_skill(&manifest).map_err(|e| anyhow!("scan skill: {e}"))?;

    // The subdir name equals the manifest name; the loader/validator require a
    // single safe path component (lowercase-kebab) here.
    let skill_name = manifest.name.clone();
    if !mur_common::skill::loader::is_valid_skill_name(&skill_name) {
        bail!("refusing to install skill with unsafe name {skill_name:?}");
    }

    let (path, mut profile) = load_profile_for_edit(name)?;

    // Write into the loadable layout: agents/<agent>/skills/<name>/skill.yaml,
    // reusing the same canonical writer the runtime loader reads from.
    let agent_home = path.parent().unwrap_or(Path::new(""));
    let dest_dir = agent_home.join("skills").join(&skill_name);
    mur_common::skill::write_to_dir(&dest_dir, &manifest)
        .map_err(|e| anyhow!("write skill to {}: {e}", dest_dir.display()))?;

    if report.has_blocking_findings() {
        eprintln!("⚠ {skill_name}: security findings — review before trusting");
        for line in report.human_summary() {
            eprintln!("    {line}");
        }
    }

    let skill_id = format!("skills/{skill_name}");
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

    // Delete the backing artifact if no other skill entry references it.
    // Modern skills are per-skill subdirectories (`skills/<name>/skill.yaml`);
    // legacy entries may still be a flat file (`skills/<name>.md`).
    let agent_home = resolve_mur_home()?.join("agents").join(name);
    let backing = agent_home.join(&resolved);
    if backing.exists() && !profile.skills.iter().any(|s| s == &resolved) {
        if backing.is_dir() {
            let _ = fs::remove_dir_all(&backing);
        } else {
            let _ = fs::remove_file(&backing);
        }
    }
    Ok(())
}

pub fn cmd_skill_show(name: &str, query: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let resolved = resolve_skill_id(&profile, query)
        .ok_or_else(|| anyhow!("skill '{query}' not registered on '{name}'"))?;
    let agent_home = resolve_mur_home()?.join("agents").join(name);
    let backing = agent_home.join(resolved);

    if backing.is_dir() {
        // Modern per-skill subdir — read the canonical manifest the loader uses.
        let m = mur_common::skill::read_from_dir(&backing)
            .map_err(|e| anyhow!("read skill {}: {e}", backing.display()))?;
        let out = mur_common::skill::serialize_canonical(&m)?;
        print!("{out}");
        return Ok(());
    }

    // Legacy flat file.
    let ext = backing.extension().and_then(|e| e.to_str()).unwrap_or("");
    if matches!(ext, "yaml" | "yml") {
        let text = std::fs::read_to_string(&backing)?;
        let m = mur_common::skill::parse_canonical(&text)?;
        let out = mur_common::skill::serialize_canonical(&m)?;
        print!("{out}");
    } else {
        // Legacy .md — print raw
        let body = std::fs::read_to_string(&backing)?;
        print!("{body}");
    }
    Ok(())
}
