//! P1a one-shot: export every pattern to markdown under
//! `~/.mur/exported-patterns/`, then delete `patterns/` and
//! `fingerprints.jsonl`. Never hard-delete user data without a backup path.

use anyhow::{Context, Result};
use std::path::Path;

pub struct MigrateReport {
    pub exported: usize,
    pub deleted_fingerprints: bool,
}

pub fn migrate_patterns_in(mur_dir: &Path) -> Result<MigrateReport> {
    let patterns_dir = mur_dir.join("patterns");
    let export_dir = mur_dir.join("exported-patterns");
    let mut exported = 0usize;

    if patterns_dir.exists() {
        std::fs::create_dir_all(&export_dir)?;
        for entry in std::fs::read_dir(&patterns_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("pattern")
                .to_string();
            let yaml = std::fs::read_to_string(&path)?;
            let md = format!(
                "# {stem}\n\nExported from `patterns/{stem}.yaml` by `mur migrate --patterns` (workflow-engine v2 P1a).\n\n```yaml\n{yaml}\n```\n"
            );
            std::fs::write(export_dir.join(format!("{stem}.md")), md)
                .context("write exported pattern")?;
            exported += 1;
        }
        std::fs::remove_dir_all(&patterns_dir)?;
    }

    let fp = mur_dir.join("fingerprints.jsonl");
    let deleted_fingerprints = fp.exists();
    if deleted_fingerprints {
        std::fs::remove_file(&fp)?;
    }
    Ok(MigrateReport {
        exported,
        deleted_fingerprints,
    })
}

pub fn cmd_migrate_patterns() -> Result<()> {
    let report = migrate_patterns_in(&crate::paths::mur_root(None))?;
    eprintln!(
        "✓ exported {} pattern(s) to ~/.mur/exported-patterns/; fingerprints.jsonl removed: {}",
        report.exported, report.deleted_fingerprints
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_then_deletes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pdir = tmp.path().join("patterns");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("a.yaml"), "name: a\n").unwrap();
        std::fs::write(tmp.path().join("fingerprints.jsonl"), "{}\n").unwrap();

        let r = migrate_patterns_in(tmp.path()).unwrap();
        assert_eq!(r.exported, 1);
        assert!(r.deleted_fingerprints);
        assert!(!pdir.exists());
        assert!(tmp.path().join("exported-patterns").join("a.md").exists());
    }

    #[test]
    fn idempotent_when_nothing_to_do() {
        let tmp = tempfile::TempDir::new().unwrap();
        let r = migrate_patterns_in(tmp.path()).unwrap();
        assert_eq!(r.exported, 0);
        assert!(!r.deleted_fingerprints);
    }
}
