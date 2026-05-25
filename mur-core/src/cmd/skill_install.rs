//! Skill install orchestrator — resolve source, fetch, verify, store, trust.

use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;

use mur_common::skill::{
    parse_canonical, content_sha256, validate,
    scan::scan_skill, write_to_dir, global_skill_dir,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};

use crate::cmd::agent::resolve_mur_home;
use crate::cmd::skill_registry;

pub fn cmd_install(source: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let src_path = Path::new(source);

    if src_path.exists() && src_path.is_file() {
        return install_from_file(&home, src_path);
    }

    // Treat as registry name
    install_from_registry(&home, source)
}

fn install_from_file(home: &Path, path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let m = parse_canonical(&text)?;
    validate(&m)?;

    let report = scan_skill(&m)?;
    if report.has_blocking_findings() {
        eprintln!("⚠ Security findings — install proceeds in Sandboxed mode:");
        for line in report.human_summary() {
            eprintln!("  {line}");
        }
    }

    let dir = global_skill_dir(home, &m.name);
    write_to_dir(&dir, &m)?;

    let hash = content_sha256(&m)?;
    let mut trust = SkillTrustStore::load(home)
        .map_err(|e| anyhow!("load trust: {e}"))?;
    let level = if report.has_blocking_findings() {
        mur_common::skill::TrustLevel::Sandboxed
    } else {
        mur_common::skill::TrustLevel::Verified
    };
    trust.insert(hash, TrustEntry {
        name: m.name.clone(),
        version: m.version.clone(),
        level,
        installed_at: chrono::Utc::now().to_rfc3339(),
        publisher: Some(m.publisher.clone()),
    });
    trust.save(home).map_err(|e| anyhow!("save trust: {e}"))?;

    println!("installed: {} v{}", m.name, m.version);
    Ok(())
}

fn install_from_registry(home: &Path, name: &str) -> Result<()> {
    let (_reg_dir, idx) = skill_registry::fetch_and_load(home, skill_registry::DEFAULT_REGISTRY)
        .context("fetch registry")?;

    let entry = idx.skills.get(name)
        .ok_or_else(|| anyhow!("skill '{name}' not found in registry"))?;

    let reg_dir = skill_registry::registry_cache_dir(home);
    let skill_path = skill_registry::skill_yaml_path(&reg_dir, name, &entry.latest);
    if !skill_path.exists() {
        bail!("skill file not found at {}", skill_path.display());
    }

    install_from_file(home, &skill_path)
}

pub fn cmd_update(name: &str) -> Result<()> {
    install_from_registry(&resolve_mur_home()?, name)?;
    println!("updated: {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_yaml_parses_and_validates() {
        let yaml = r#"
name: test
version: 1.0.0
publisher: human:t
description: t
category: context
content:
  abstract: a
  context: b
"#;
        let m = parse_canonical(yaml).unwrap();
        validate(&m).unwrap();
    }
}
