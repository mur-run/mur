//! Skill install orchestrator — resolve source, fetch, verify, store, trust, lock.

use anyhow::{Context, Result, bail};
use std::path::Path;

use mur_common::skill::{
    content_sha256, global_skill_dir, scan::scan_skill, write_to_dir, TrustLevel,
};
use mur_common::skill::{SkillLock, lockfile};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};

use crate::cmd::agent::resolve_mur_home;
use crate::cmd::skill_registry;
use crate::cmd::skill_resolver::{self, ResolveSource, ResolvedNode, ResolverInput};

/// Pure entry point — takes explicit home + registry_url. Used by tests and future M4 code.
pub fn cmd_install(home: &Path, registry_url: &str, source: &str) -> Result<()> {
    let src_path = Path::new(source);

    let (reg_dir, _idx) = skill_registry::fetch_and_load(home, registry_url)
        .context("fetch registry")?;

    let input = ResolverInput {
        mur_home: home.to_path_buf(),
        registry_dir: reg_dir,
    };

    let source_enum = if src_path.exists() && src_path.is_file() {
        ResolveSource::LocalFile(src_path)
    } else {
        ResolveSource::RegistryLatest(source)
    };

    let order = skill_resolver::resolve(&input, source_enum)?;
    if order.is_empty() {
        bail!("resolver returned empty install order");
    }

    // Install leaves first. The root is the last entry.
    for node in &order {
        install_resolved_node(home, node)?;
    }

    // Write lock at the root skill dir.
    let root = order.last().unwrap();
    let lock = SkillLock {
        schema_version: lockfile::SCHEMA_VERSION,
        installed_at: chrono::Utc::now().to_rfc3339(),
        locked: order
            .iter()
            .map(|n| (n.name.clone(), n.version.to_string()))
            .collect(),
    };
    let root_dir = global_skill_dir(home, &root.name);
    lock.write(&root_dir).context("write skill.lock")?;

    println!("installed: {} v{}", root.name, root.version);
    if order.len() > 1 {
        println!("  + {} transitive dependencies", order.len() - 1);
    }
    Ok(())
}

fn install_resolved_node(home: &Path, node: &ResolvedNode) -> Result<()> {
    let report = scan_skill(&node.manifest)?;
    let dir = global_skill_dir(home, &node.name);
    write_to_dir(&dir, &node.manifest)?;
    let hash = content_sha256(&node.manifest)?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow::anyhow!("load trust: {e}"))?;
    let level = if report.has_blocking_findings() {
        TrustLevel::Sandboxed
    } else {
        TrustLevel::Verified
    };
    trust.insert(
        hash,
        TrustEntry {
            name: node.name.clone(),
            version: node.version.to_string(),
            level,
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(node.manifest.publisher.clone()),
        },
    );
    trust
        .save(home)
        .map_err(|e| anyhow::anyhow!("save trust: {e}"))?;
    if report.has_blocking_findings() {
        eprintln!(
            "⚠ {} v{}: security findings — installed Sandboxed",
            node.name, node.version
        );
        for line in report.human_summary() {
            eprintln!("    {line}");
        }
    }
    Ok(())
}

/// CLI shim — resolves MUR_HOME and MUR_SKILL_REGISTRY_URL from env.
pub fn cmd_install_cli(source: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
        .unwrap_or_else(|_| skill_registry::DEFAULT_REGISTRY.to_string());
    cmd_install(&home, &registry_url, source)
}

/// Pure update — re-resolves to latest versions.
pub fn cmd_update(home: &Path, registry_url: &str, name: &str) -> Result<()> {
    cmd_install(home, registry_url, name)?;
    println!("updated: {name}");
    Ok(())
}

/// CLI shim for update.
pub fn cmd_update_cli(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
        .unwrap_or_else(|_| skill_registry::DEFAULT_REGISTRY.to_string());
    cmd_update(&home, &registry_url, name)
}

#[cfg(test)]
mod tests {
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
        let m = mur_common::skill::parse_canonical(yaml).unwrap();
        mur_common::skill::validate(&m).unwrap();
    }
}
