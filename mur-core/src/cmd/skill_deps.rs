//! `mur skill deps <name>` — print the resolved dependency tree from `skill.lock`.

use anyhow::{Context, Result, bail};
use mur_common::skill::{SkillLock, Requirement, global_skill_dir, local::load_installed};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use crate::cmd::agent::resolve_mur_home;

pub fn cmd_deps(home: &Path, name: &str) -> Result<()> {
    cmd_deps_to(home, name, &mut std::io::stdout().lock())
}

pub fn cmd_deps_to(home: &Path, name: &str, w: &mut dyn Write) -> Result<()> {
    let root_dir = global_skill_dir(home, name);
    if !root_dir.exists() {
        bail!("'{name}' is not installed");
    }
    let lock = SkillLock::read(&root_dir).context("read skill.lock")?;
    let root_manifest = load_installed(home, name).context("read root manifest")?;

    writeln!(w, "{} v{}", name, root_manifest.version)?;
    print_subtree(w, &lock.locked, &root_manifest.requires, "  ")?;
    Ok(())
}

/// CLI shim — resolves MUR_HOME from env.
pub fn cmd_deps_cli(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    cmd_deps(&home, name)
}

fn print_subtree(
    w: &mut dyn Write,
    locked: &BTreeMap<String, String>,
    reqs: &[Requirement],
    indent: &str,
) -> Result<()> {
    for r in reqs {
        let pinned = locked.get(&r.name).map(String::as_str).unwrap_or("?");
        writeln!(w, "{indent}{} ({}) -> {pinned}", r.name, r.version)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::{SkillLock, lockfile};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn not_installed() {
        let home = tempdir().unwrap();
        let err = cmd_deps(home.path(), "nonexistent").unwrap_err();
        assert!(
            err.to_string().contains("not installed"),
            "expected not-installed, got: {err}"
        );
    }

    #[test]
    fn prints_lock_contents() {
        let home = tempdir().unwrap();
        let skill_dir = global_skill_dir(home.path(), "test-skill");
        fs::create_dir_all(&skill_dir).unwrap();

        let skill_yaml = r#"
name: test-skill
version: 1.0.0
publisher: human:test
description: test
category: context
requires:
  - name: dep-a
    version: ">=1.0.0"
  - name: dep-b
    version: "^2.0.0"
content:
  abstract: a
  context: b
"#;
        fs::write(skill_dir.join("skill.yaml"), skill_yaml).unwrap();

        let mut lock = SkillLock {
            schema_version: lockfile::SCHEMA_VERSION,
            installed_at: "2026-05-25T00:00:00Z".into(),
            locked: BTreeMap::new(),
        };
        lock.locked.insert("test-skill".into(), "1.0.0".into());
        lock.locked.insert("dep-a".into(), "1.2.0".into());
        lock.locked.insert("dep-b".into(), "2.5.0".into());
        lock.write(&skill_dir).unwrap();

        let mut buf = Vec::new();
        cmd_deps_to(home.path(), "test-skill", &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();

        assert!(out.contains("test-skill v1.0.0"), "output: {out}");
        assert!(out.contains("dep-a (>=1.0.0) -> 1.2.0"), "output: {out}");
        assert!(out.contains("dep-b (^2.0.0) -> 2.5.0"), "output: {out}");
    }
}
