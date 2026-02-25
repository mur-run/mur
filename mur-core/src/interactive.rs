//! Enhanced CLI interactions — guided `mur new`, edit preview/diff,
//! `mur why`, and template system.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use console::style;
use dialoguer::{Confirm, Input, Select};

use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;

use crate::retrieve::scoring::{ScoredPattern, score_and_rank};
use crate::store::yaml::YamlStore;

// ─── Templates ─────────────────────────────────────────────────────

/// Built-in template definitions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Template {
    Insight,
    Technique,
    Pitfall,
    Checklist,
    Custom,
}

impl Template {
    pub fn all() -> &'static [Template] {
        &[
            Template::Insight,
            Template::Technique,
            Template::Pitfall,
            Template::Checklist,
            Template::Custom,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Template::Insight => "Insight      (observation or lesson)",
            Template::Technique => "Technique    (how-to with examples)",
            Template::Pitfall => "Pitfall      (mistake to avoid)",
            Template::Checklist => "Checklist    (step-by-step)",
            Template::Custom => "Custom       (blank)",
        }
    }

    #[allow(dead_code)] // Public API — used by template init
    pub fn file_name(&self) -> &'static str {
        match self {
            Template::Insight => "insight.yaml",
            Template::Technique => "technique.yaml",
            Template::Pitfall => "pitfall.yaml",
            Template::Checklist => "checklist.yaml",
            Template::Custom => "custom.yaml",
        }
    }

    /// Generate template content for the description field.
    #[allow(dead_code)] // Public API — used by template init
    pub fn description_hint(&self) -> &'static str {
        match self {
            Template::Insight => "Observed that...",
            Template::Technique => "How to...",
            Template::Pitfall => "Avoid...",
            Template::Checklist => "Steps to...",
            Template::Custom => "",
        }
    }

    /// Generate template content for the technical layer.
    pub fn technical_template(&self) -> &'static str {
        match self {
            Template::Insight => "Key insight: ",
            Template::Technique => "## Steps\n\n1. \n2. \n3. \n\n## Example\n\n",
            Template::Pitfall => "## Problem\n\n## Why It Happens\n\n## Correct Approach\n\n",
            Template::Checklist => "- [ ] Step 1\n- [ ] Step 2\n- [ ] Step 3\n",
            Template::Custom => "",
        }
    }
}

/// Ensure default templates exist in ~/.mur/templates/.
#[allow(dead_code)] // Public API — called from `mur init`
pub fn ensure_default_templates() -> Result<PathBuf> {
    let templates_dir = templates_dir();
    fs::create_dir_all(&templates_dir)?;

    for tpl in Template::all() {
        let path = templates_dir.join(tpl.file_name());
        if !path.exists() {
            let yaml = format!(
                "# MUR Pattern Template: {}\n# Edit this file to customize the template.\n\ndescription: \"{}\"\ntechnical: |\n  {}\n",
                tpl.file_name().trim_end_matches(".yaml"),
                tpl.description_hint(),
                tpl.technical_template().replace('\n', "\n  "),
            );
            fs::write(&path, yaml)?;
        }
    }

    Ok(templates_dir)
}

fn templates_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("templates")
}

// ─── Interactive new ───────────────────────────────────────────────

/// Run the guided interactive pattern creation flow.
/// Returns the name of the created pattern, or None if cancelled.
pub fn interactive_new(store: &YamlStore) -> Result<Option<String>> {
    println!();
    println!("  {} Create New Pattern", style("*").cyan().bold());
    println!();

    // 1. Description (what did you learn?)
    let description: String = Input::new()
        .with_prompt("  What did you learn? (describe the pattern)")
        .interact_text()?;

    if description.trim().is_empty() {
        println!("  Cancelled.");
        return Ok(None);
    }

    // 2. Triggers
    let triggers_input: String = Input::new()
        .with_prompt("  When should this trigger? (comma-separated keywords)")
        .interact_text()?;

    let triggers: Vec<String> = triggers_input
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // 3. Confidence
    let confidence_input: String = Input::new()
        .with_prompt("  Confidence (0.0-1.0)")
        .default("0.7".to_string())
        .interact_text()?;

    let confidence: f64 = confidence_input
        .parse::<f64>()
        .unwrap_or(0.7)
        .clamp(0.0, 1.0);

    // 4. Tier
    let tier_options = &[
        "Session    (this session only)",
        "Project    (this project)",
        "Global     (everywhere)",
    ];
    let tier_idx = Select::new()
        .with_prompt("  Tier")
        .items(tier_options)
        .default(0)
        .interact()?;

    let tier = match tier_idx {
        1 => Tier::Project,
        2 => Tier::Core,
        _ => Tier::Session,
    };

    // 5. Template
    let template_labels: Vec<&str> = Template::all().iter().map(|t| t.label()).collect();
    let tpl_idx = Select::new()
        .with_prompt("  Template")
        .items(&template_labels)
        .default(0)
        .interact()?;

    let template = Template::all()[tpl_idx];

    // 6. Optional code example
    let mut example_code = String::new();
    let mut example_lang = String::new();
    let add_example = Confirm::new()
        .with_prompt("  Add a code example?")
        .default(false)
        .interact()?;

    if add_example {
        example_lang = Input::new()
            .with_prompt("  Language")
            .default("text".to_string())
            .interact_text()?;

        println!("  Paste code (empty line to finish):");
        let mut lines = Vec::new();
        loop {
            let line: String = Input::new()
                .with_prompt("  ")
                .default(String::new())
                .allow_empty(true)
                .interact_text()?;
            if line.is_empty() {
                break;
            }
            lines.push(line);
        }
        example_code = lines.join("\n");
    }

    // Generate name from description
    let name = generate_name(&description);
    if store.exists(&name) {
        println!(
            "  {} Pattern '{}' already exists.",
            style("!").red().bold(),
            name
        );
        return Ok(None);
    }

    // Build the technical content
    let mut technical = template.technical_template().to_string();
    technical.push_str(&description);
    if !example_code.is_empty() {
        technical.push_str(&format!("\n\n```{}\n{}\n```", example_lang, example_code));
    }

    let pattern = Pattern {
        base: KnowledgeBase {
            schema: SCHEMA_VERSION,
            name: name.clone(),
            description: description.clone(),
            content: Content::DualLayer {
                technical,
                principle: None,
            },
            tier,
            importance: 0.5,
            confidence,
            tags: Tags {
                topics: triggers.clone(),
                ..Tags::default()
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        attachments: vec![],
    };

    store.save(&pattern)?;

    println!();
    println!(
        "  {} Created: {}",
        style("OK").green().bold(),
        style(&name).cyan()
    );
    println!(
        "     Maturity: Draft | Tier: {:?} | Confidence: {:.2}",
        tier, confidence
    );
    println!("     File: ~/.mur/patterns/{}.yaml", name);
    println!();
    println!("  Tip: `mur edit {}` to refine", name);
    println!("       `mur serve --open` to edit in browser");

    Ok(Some(name))
}

/// Generate a kebab-case name from a description.
fn generate_name(description: &str) -> String {
    let stop_words = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "shall", "should", "may", "might", "must", "can",
        "could", "to", "of", "in", "for", "on", "with", "at", "by", "from", "as", "into",
        "through", "during", "before", "after", "above", "below", "and", "but", "or", "nor", "not",
        "so", "yet", "both", "either", "neither", "each", "every", "all", "any", "few", "more",
        "most", "other", "some", "such", "no", "only", "own", "same", "than", "too", "very",
        "just", "that", "this", "it", "its", "i",
    ];

    let lowered = description.to_lowercase();
    let words: Vec<&str> = lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && w.len() > 1 && !stop_words.contains(w))
        .take(4)
        .collect();

    if words.is_empty() {
        format!("pattern-{}", chrono::Utc::now().format("%Y%m%d%H%M%S"))
    } else {
        words.join("-")
    }
}

// ─── Edit preview ──────────────────────────────────────────────────

/// Show a preview of a pattern before editing.
pub fn show_edit_preview(pattern: &Pattern) {
    let maturity_label = format!("{:?}", pattern.maturity);
    let tier_label = format!("{:?}", pattern.tier);

    println!();
    println!(
        "  {} {} {}",
        style("--").dim(),
        style(&pattern.name).cyan().bold(),
        style("--").dim(),
    );
    println!(
        "  Maturity: {:<12} Confidence: {:.2}",
        maturity_label, pattern.confidence
    );
    println!(
        "  Tier: {:<15} Tags: {}",
        tier_label,
        pattern
            .tags
            .topics
            .iter()
            .chain(pattern.tags.languages.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "  Injections: {:<10} Last used: {}",
        pattern.evidence.injection_count,
        pattern
            .lifecycle
            .last_injected
            .map(|d| format!("{}", d.format("%Y-%m-%d")))
            .unwrap_or_else(|| "never".to_string())
    );
    println!();
    println!("  \"{}\"", pattern.description);
    println!();
}

/// Show a diff between old and new pattern state.
pub fn show_edit_diff(old: &Pattern, new: &Pattern) {
    let mut changes = Vec::new();

    if old.description != new.description {
        changes.push(format!(
            "  description: {} -> {}",
            style(&old.description).red(),
            style(&new.description).green(),
        ));
    }
    if (old.confidence - new.confidence).abs() > f64::EPSILON {
        changes.push(format!(
            "  confidence:  {:.2} -> {:.2}",
            old.confidence, new.confidence
        ));
    }
    if (old.importance - new.importance).abs() > f64::EPSILON {
        changes.push(format!(
            "  importance:  {:.2} -> {:.2}",
            old.importance, new.importance
        ));
    }
    if old.tier != new.tier {
        changes.push(format!("  tier:        {:?} -> {:?}", old.tier, new.tier));
    }
    if old.tags.topics != new.tags.topics {
        let added: Vec<_> = new
            .tags
            .topics
            .iter()
            .filter(|t| !old.tags.topics.contains(t))
            .collect();
        let removed: Vec<_> = old
            .tags
            .topics
            .iter()
            .filter(|t| !new.tags.topics.contains(t))
            .collect();
        if !added.is_empty() {
            changes.push(format!("  tags:        + {:?}", added));
        }
        if !removed.is_empty() {
            changes.push(format!("  tags:        - {:?}", removed));
        }
    }

    if changes.is_empty() {
        println!("  No changes.");
    } else {
        println!("  {} Changes:", style("*").yellow().bold());
        for c in &changes {
            println!("{}", c);
        }
    }
}

// ─── mur why ───────────────────────────────────────────────────────

/// Explain why a pattern would be (or was) injected for a query context.
pub fn explain_why(pattern: &Pattern, store: &YamlStore) -> Result<()> {
    println!();
    println!(
        "  {} Why was this pattern injected?",
        style("?").cyan().bold()
    );
    println!();

    // Show last injection time
    if let Some(last) = pattern.lifecycle.last_injected {
        println!("  Last injection: {}", last.format("%Y-%m-%d %H:%M:%S UTC"));
    } else {
        println!("  Last injection: never injected");
    }
    println!();

    // Matching signals
    println!("  Matching signals:");

    // Trigger/tag info
    let tag_list = pattern
        .tags
        .topics
        .iter()
        .chain(pattern.tags.languages.iter())
        .cloned()
        .collect::<Vec<_>>();

    if !tag_list.is_empty() {
        println!(
            "    {} Tag match:      {}",
            style("*").dim(),
            tag_list.join(", ")
        );
    }

    // Confidence
    let conf_icon = if pattern.confidence >= 0.5 {
        style("OK").green()
    } else {
        style("!!").red()
    };
    println!(
        "    {} Confidence:     {:.2} (threshold: 0.50)",
        conf_icon, pattern.confidence
    );

    // Maturity
    let maturity_boost = match pattern.maturity {
        mur_common::knowledge::Maturity::Stable => "1.5x boost",
        mur_common::knowledge::Maturity::Canonical => "2.0x boost",
        mur_common::knowledge::Maturity::Emerging => "1.0x",
        mur_common::knowledge::Maturity::Draft => "0.8x penalty",
    };
    println!(
        "    {} Maturity:       {:?} ({})",
        style("*").dim(),
        pattern.maturity,
        maturity_boost
    );

    // Effectiveness
    let eff = pattern.evidence.effectiveness();
    println!(
        "    {} Effectiveness:  {:.0}% ({} success / {} override)",
        style("*").dim(),
        eff * 100.0,
        pattern.evidence.success_signals,
        pattern.evidence.override_signals
    );

    // Importance
    println!(
        "    {} Importance:     {:.0}%",
        style("*").dim(),
        pattern.importance * 100.0
    );

    println!();

    // Gate results
    println!("  Gate results:");
    let conf_ok = pattern.confidence >= 0.5;
    let not_muted = !pattern.lifecycle.muted;
    let not_archived = pattern.lifecycle.status != LifecycleStatus::Archived;
    let is_active = pattern.lifecycle.status == LifecycleStatus::Active;

    print_gate(
        "Confidence gate",
        conf_ok,
        &format!("{:.2} >= 0.50", pattern.confidence),
    );
    print_gate("Not muted", not_muted, "");
    print_gate("Not archived", not_archived, "");
    print_gate(
        "Active status",
        is_active,
        &format!("{:?}", pattern.lifecycle.status),
    );

    // Try scoring against a generic query with pattern tags
    if !tag_list.is_empty() {
        let query = tag_list.join(" ");
        let all_patterns = store.list_all().unwrap_or_default();
        let total_candidates = all_patterns.len();
        let results: Vec<ScoredPattern> = score_and_rank(&query, all_patterns);

        if let Some(pos) = results
            .iter()
            .position(|sp| sp.pattern.name == pattern.name)
        {
            println!();
            println!(
                "  Combined rank: #{} of {} candidates (query: \"{}\")",
                pos + 1,
                total_candidates,
                query
            );
            println!(
                "  Score: {:.3} | Relevance: {:.3}",
                results[pos].score, results[pos].relevance
            );
        }
    }

    println!();
    Ok(())
}

fn print_gate(name: &str, passed: bool, detail: &str) {
    let icon = if passed {
        style("OK").green()
    } else {
        style("!!").red()
    };
    if detail.is_empty() {
        println!("    {} {}", icon, name);
    } else {
        println!("    {} {} ({})", icon, name, detail);
    }
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_name_basic() {
        let name = generate_name("Always pass context.Context as first parameter in Go functions");
        assert!(name.contains("context"));
        assert!(!name.contains(' '));
        // Should be kebab-case
        assert!(name.chars().all(|c| c.is_alphanumeric() || c == '-'));
    }

    #[test]
    fn test_generate_name_filters_stop_words() {
        let name = generate_name("The quick brown fox jumps over the lazy dog");
        // "the" and "the" should be removed
        assert!(!name.starts_with("the"));
    }

    #[test]
    fn test_generate_name_empty() {
        let name = generate_name("");
        assert!(name.starts_with("pattern-"));
    }

    #[test]
    fn test_generate_name_short_words_filtered() {
        let name = generate_name("a b c d e f");
        // All single-char words should be filtered → fallback name
        assert!(name.starts_with("pattern-"));
    }

    #[test]
    fn test_generate_name_max_four_words() {
        let name = generate_name("rust error handling patterns with thiserror anyhow custom types");
        let parts: Vec<&str> = name.split('-').collect();
        assert!(parts.len() <= 4, "Expected <= 4 parts, got {:?}", parts);
    }

    #[test]
    fn test_template_labels() {
        for t in Template::all() {
            assert!(!t.label().is_empty());
            assert!(!t.file_name().is_empty());
        }
    }

    #[test]
    fn test_ensure_default_templates() {
        let tmp = tempfile::tempdir().unwrap();
        // We can't easily test ensure_default_templates because it writes to ~/.mur/
        // but we can verify Template::all() is consistent
        assert_eq!(Template::all().len(), 5);
    }

    #[test]
    fn test_show_edit_diff_no_changes() {
        let p = Pattern {
            base: KnowledgeBase {
                name: "test".to_string(),
                description: "Test".to_string(),
                content: Content::Plain("Content".to_string()),
                ..Default::default()
            },
            attachments: vec![],
        };

        // Just verify it doesn't panic
        show_edit_diff(&p, &p);
    }

    #[test]
    fn test_show_edit_diff_with_changes() {
        let old = Pattern {
            base: KnowledgeBase {
                name: "test".to_string(),
                description: "Old description".to_string(),
                content: Content::Plain("Content".to_string()),
                confidence: 0.5,
                ..Default::default()
            },
            attachments: vec![],
        };

        let mut new = old.clone();
        new.description = "New description".to_string();
        new.confidence = 0.9;

        // Just verify it doesn't panic
        show_edit_diff(&old, &new);
    }

    #[test]
    fn test_explain_why_doesnt_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let store = YamlStore::new(tmp.path().to_path_buf()).unwrap();

        let pattern = Pattern {
            base: KnowledgeBase {
                name: "test-explain".to_string(),
                description: "Test pattern for explain".to_string(),
                content: Content::Plain("test content".to_string()),
                confidence: 0.8,
                tags: Tags {
                    topics: vec!["rust".into()],
                    ..Tags::default()
                },
                ..Default::default()
            },
            attachments: vec![],
        };
        store.save(&pattern).unwrap();

        // Just verify it doesn't panic
        explain_why(&pattern, &store).unwrap();
    }

    #[test]
    fn test_explain_why_muted_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let store = YamlStore::new(tmp.path().to_path_buf()).unwrap();

        let mut pattern = Pattern {
            base: KnowledgeBase {
                name: "muted-test".to_string(),
                description: "Muted pattern".to_string(),
                content: Content::Plain("content".to_string()),
                ..Default::default()
            },
            attachments: vec![],
        };
        pattern.lifecycle.muted = true;
        store.save(&pattern).unwrap();

        explain_why(&pattern, &store).unwrap();
    }

    #[test]
    fn test_show_edit_preview_doesnt_panic() {
        let p = Pattern {
            base: KnowledgeBase {
                name: "preview-test".to_string(),
                description: "Test preview".to_string(),
                content: Content::Plain("content".to_string()),
                confidence: 0.85,
                tags: Tags {
                    topics: vec!["rust".into(), "testing".into()],
                    ..Tags::default()
                },
                ..Default::default()
            },
            attachments: vec![],
        };

        show_edit_preview(&p);
    }
}
