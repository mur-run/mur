//! `mur notes` CLI handlers — MVP create + search.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::skill::manifest::{Content, SkillManifest};
use mur_common::skill::store::{global_skill_dir, write_to_dir};
use mur_common::skill::types::{Category, Priority};
use mur_common::skill::validate;

/// Author identity stamped onto notes created via the local CLI.
/// Plan-marker: later plans may swap this for a config-driven value.
const DEFAULT_PUBLISHER: &str = "human:local";

/// Build a `category: note` skill at `<mur_home>/skills/<name>/skill.yaml`.
/// Returns the path written.
///
/// Errors:
/// - if the target skill directory already contains a `skill.yaml` (duplicate name)
/// - if the resulting manifest fails `mur_common::skill::validate::validate`
pub fn do_create(
    mur_home: &Path,
    name: &str,
    description: &str,
    body: &str,
) -> Result<PathBuf> {
    let dir = global_skill_dir(mur_home, name);
    if dir.join("skill.yaml").exists() {
        bail!("note '{name}' already exists at {}", dir.display());
    }

    let manifest = SkillManifest {
        name: name.to_string(),
        version: "1.0.0".into(),
        publisher: DEFAULT_PUBLISHER.into(),
        description: description.to_string(),
        category: Category::Note,
        hosts: vec![],
        content: Content {
            r#abstract: description.to_string(),
            context: None,
            procedure: None,
            command: None,
            note: Some(body.to_string()),
        },
        requires: vec![],
        tags: vec![],
        triggers: vec![],
        priority: Priority::Normal,
        evolution_log: vec![],
        transfer_chain: vec![],
        mcp_requirements: vec![],
    };

    validate(&manifest).with_context(|| format!("validate note '{name}'"))?;
    let written = write_to_dir(&dir, &manifest)
        .with_context(|| format!("write skill.yaml for '{name}'"))?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::parser::parse_canonical;
    use tempfile::tempdir;

    #[test]
    fn do_create_writes_a_well_formed_note_skill() {
        let tmp = tempdir().unwrap();
        let path = do_create(
            tmp.path(),
            "rust-error-handling",
            "Rust error handling reference",
            "# Rust Error Handling\n\nUse anyhow for app errors.",
        )
        .unwrap();

        assert!(path.ends_with("skills/rust-error-handling/skill.yaml"));
        let yaml = std::fs::read_to_string(&path).unwrap();
        let m = parse_canonical(&yaml).unwrap();

        assert_eq!(m.name, "rust-error-handling");
        assert_eq!(m.category, Category::Note);
        assert_eq!(m.content.r#abstract, "Rust error handling reference");
        assert_eq!(
            m.content.note.as_deref(),
            Some("# Rust Error Handling\n\nUse anyhow for app errors.")
        );
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn do_create_rejects_duplicate_name() {
        let tmp = tempdir().unwrap();
        do_create(tmp.path(), "dup", "d", "body").unwrap();
        let err = do_create(tmp.path(), "dup", "d", "body").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn do_create_rejects_invalid_name() {
        let tmp = tempdir().unwrap();
        // Uppercase letters violate validate_name (ascii_lowercase only).
        let err = do_create(tmp.path(), "BadName", "d", "body").unwrap_err();
        assert!(err.to_string().contains("validate") || err.to_string().contains("name"));
    }
}
