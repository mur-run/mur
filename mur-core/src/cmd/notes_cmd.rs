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

use std::io::Read;

use super::agent::resolve_mur_home;
use crate::retrieve::scoring::{Scored, score_and_rank_generic};
use crate::retrieve::skill_candidates::{LoadedSkill, load_skill_candidates};

/// Top-level `mur notes create` handler.
pub fn cmd_create(name: &str, description: &str, body_file: Option<&Path>) -> Result<()> {
    let body = match body_file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("read body file {}", p.display()))?,
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("read body from stdin")?;
            s
        }
    };
    let home = resolve_mur_home()?;
    let path = do_create(&home, name, description, &body)?;
    println!("Created note '{}' at {}", name, path.display());
    Ok(())
}

/// Top-level `mur notes search` handler.
pub fn cmd_search(query: &str, limit: usize) -> Result<()> {
    let home = resolve_mur_home()?;
    let ranked = do_search(&home, query, limit)?;
    if ranked.is_empty() {
        println!("No notes match '{query}'.");
        return Ok(());
    }
    for (i, sp) in ranked.iter().enumerate() {
        println!(
            "{:>2}. {:<40} score={:.3}  {}",
            i + 1,
            sp.item.manifest.name,
            sp.score,
            sp.item.manifest.description
        );
    }
    Ok(())
}

/// Search `~/.mur/skills/` for `category: note` skills matching `query`.
/// Returns up to `limit` ranked results (Scored<LoadedSkill>).
pub fn do_search(
    mur_home: &Path,
    query: &str,
    limit: usize,
) -> Result<Vec<Scored<LoadedSkill>>> {
    let skills_dir = mur_home.join("skills");
    let all = load_skill_candidates(&skills_dir, mur_home)?;
    let notes: Vec<LoadedSkill> = all
        .into_iter()
        .filter(|s| s.manifest.category == Category::Note)
        .collect();
    let mut ranked = score_and_rank_generic(query, notes);
    ranked.truncate(limit);
    Ok(ranked)
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

    #[test]
    fn do_search_filters_out_non_note_skills() {
        use std::fs;
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");

        // A genuine note (created via do_create).
        do_create(tmp.path(), "deploy-fly", "Deploy to Fly.io", "# fly deploy steps").unwrap();

        // A non-note (category: context) hand-written to the same skills dir.
        let ctx_dir = skills_dir.join("context-thing");
        fs::create_dir_all(&ctx_dir).unwrap();
        fs::write(
            ctx_dir.join("skill.yaml"),
            "name: context-thing\nversion: 1.0.0\npublisher: human:test\n\
             category: context\ndescription: deploy context\n\
             content:\n  abstract: deploy fly\n  context: details\n",
        )
        .unwrap();

        let ranked = do_search(tmp.path(), "deploy fly", 10).unwrap();
        let names: Vec<_> = ranked.iter().map(|s| s.item.manifest.name.clone()).collect();
        assert!(names.contains(&"deploy-fly".to_string()));
        assert!(!names.contains(&"context-thing".to_string()));
    }

    #[test]
    fn do_search_respects_limit_and_orders_by_score() {
        let tmp = tempdir().unwrap();
        do_create(
            tmp.path(),
            "rust-anyhow",
            "Anyhow for rust apps",
            "# anyhow\nuse anyhow for application errors",
        )
        .unwrap();
        do_create(
            tmp.path(),
            "rust-thiserror",
            "thiserror for libraries",
            "# thiserror\nuse thiserror for library errors",
        )
        .unwrap();
        do_create(
            tmp.path(),
            "unrelated-brew",
            "homebrew update",
            "# brew\nrun brew update weekly",
        )
        .unwrap();

        let ranked = do_search(tmp.path(), "rust anyhow application errors", 2).unwrap();
        assert!(ranked.len() <= 2);
        assert_eq!(
            ranked[0].item.manifest.name, "rust-anyhow",
            "rust-anyhow should rank above rust-thiserror for this query"
        );
        if ranked.len() == 2 {
            assert!(ranked[0].score >= ranked[1].score);
        }
    }

    #[test]
    fn do_search_returns_empty_when_no_notes_exist() {
        let tmp = tempdir().unwrap();
        let ranked = do_search(tmp.path(), "anything", 10).unwrap();
        assert!(ranked.is_empty());
    }
}
