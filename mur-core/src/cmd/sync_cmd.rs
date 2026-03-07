use anyhow::Result;
use mur_common::pattern::*;

use crate::capture;
use crate::inject;
use crate::store::yaml::YamlStore;

pub(crate) fn cmd_sync(quiet: bool, project_aware: bool) -> Result<()> {
    use crate::evolve::decay::apply_decay_all;
    use crate::evolve::maturity::apply_maturity_all;
    use crate::retrieve::scoring::score_and_rank;
    use inject::sync::{default_targets, generate_sync_content, write_sync_file};

    let store = YamlStore::default_store()?;
    let now = chrono::Utc::now();

    // Run decay + maturity before syncing
    let _ = apply_decay_all(&store, now)?;
    let _ = apply_maturity_all(&store, now)?;

    let patterns = store.list_all()?;

    if patterns.is_empty() {
        if !quiet {
            println!("No patterns to sync.");
        }
        return Ok(());
    }

    // Get current working directory for project-scoped sync
    let cwd = std::env::current_dir()?;
    let targets = default_targets();

    // Build project-aware query when --project is set
    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let sync_query = if project_aware {
        build_project_sync_query(&cwd, &project_name)
    } else {
        project_name.clone()
    };

    for target in &targets {
        let target_path = cwd.join(&target.file);

        // Only write to files that already exist on disk
        if !target_path.exists() {
            continue;
        }

        let mut scored = score_and_rank(&sync_query, patterns.clone());

        // When project-aware, additionally boost patterns whose applies.projects
        // or tags.languages match the current project
        if project_aware {
            let detected_lang = capture::starter::detect_language_name(&cwd);
            for sp in &mut scored {
                let p = &sp.pattern;
                // Boost patterns that explicitly list this project
                if p.applies
                    .projects
                    .iter()
                    .any(|proj| proj == &project_name || proj == "*")
                {
                    sp.score *= 1.3;
                }
                // Boost patterns matching detected language
                if let Some(ref lang) = detected_lang {
                    let lang_lower = lang.to_lowercase();
                    if p.tags
                        .languages
                        .iter()
                        .any(|l| l.to_lowercase() == lang_lower)
                        || p.applies
                            .languages
                            .iter()
                            .any(|l| l.to_lowercase() == lang_lower)
                    {
                        sp.score *= 1.2;
                    }
                }
            }
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let top: Vec<Pattern> = scored
            .into_iter()
            .take(target.max_patterns)
            .map(|sp| sp.pattern)
            .collect();

        if top.is_empty() {
            continue;
        }

        let content = generate_sync_content(&top, &target.format);
        write_sync_file(&target_path, &content, &target.format)?;
        if !quiet {
            println!(
                "  {} — wrote {} patterns to {}",
                target.name,
                top.len(),
                target_path.display()
            );
        }
    }

    // ─── Ensure skills are installed ───────────────────────────
    let home = dirs::home_dir().expect("no home dir");
    let skill_installed = ensure_mur_skill(&home)?;
    if !quiet && skill_installed {
        println!("  🎓 MUR skill installed/updated for AI tools");
    }

    if !quiet {
        println!("Sync complete.");
    }
    Ok(())
}

/// Install/update the MUR skill for AI tools that support skills.
/// Returns true if any skill was written.
pub(crate) fn ensure_mur_skill(home: &std::path::Path) -> Result<bool> {
    let skill_content = include_str!("../mur_skill.md");

    // Claude Code: ~/.claude/skills/mur/
    if home.join(".claude").exists() {
        let claude_skill = home.join(".claude").join("skills").join("mur");
        std::fs::create_dir_all(&claude_skill)?;
        std::fs::write(claude_skill.join("SKILL.md"), skill_content)?;
    }

    // Auggie: ~/.augment/skills/mur/
    if home.join(".augment").exists() {
        let auggie_skill = home.join(".augment").join("skills").join("mur");
        std::fs::create_dir_all(&auggie_skill)?;
        std::fs::write(auggie_skill.join("SKILL.md"), skill_content)?;
    }

    // OpenClaw: ~/.agents/skills/mur/
    let agents_skill = home.join(".agents").join("skills").join("mur");
    std::fs::create_dir_all(&agents_skill)?;
    std::fs::write(agents_skill.join("SKILL.md"), skill_content)?;

    Ok(true)
}

/// Build a richer query for project-aware sync by detecting language and git context.
pub(crate) fn build_project_sync_query(cwd: &std::path::Path, project_name: &str) -> String {
    let mut parts = vec![project_name.to_string()];

    // Detect language
    if let Some(lang) = capture::starter::detect_language_name(cwd) {
        parts.push(lang);
    }

    // Try git remote for extra context
    if let Ok(output) = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(name) = remote.rsplit('/').next() {
            let name = name.trim_end_matches(".git");
            if name != project_name {
                parts.push(name.to_string());
            }
        }
    }

    parts.join(" ")
}
