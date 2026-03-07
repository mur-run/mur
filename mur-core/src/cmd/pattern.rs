use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::io::{self, Write};

use crate::evolve;
use crate::interactive;
use crate::store::yaml::YamlStore;

pub(crate) fn cmd_new(diagram_path: Option<String>) -> Result<()> {
    let store = YamlStore::default_store()?;

    // Use interactive mode when on a TTY and no diagram given
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) && diagram_path.is_none() {
        let _ = interactive::interactive_new(&store)?;
        return Ok(());
    }

    print!("Pattern name (kebab-case): ");
    io::stdout().flush()?;
    let mut name = String::new();
    io::stdin().read_line(&mut name)?;
    let name = name.trim().to_string();

    if name.is_empty() {
        println!("❌ Name cannot be empty.");
        return Ok(());
    }
    if store.exists(&name) {
        println!("❌ Pattern '{}' already exists.", name);
        return Ok(());
    }

    print!("Description: ");
    io::stdout().flush()?;
    let mut desc = String::new();
    io::stdin().read_line(&mut desc)?;
    let desc = desc.trim().to_string();

    println!("Technical content (end with empty line):");
    io::stdout().flush()?;
    let mut technical = crate::read_multiline()?;
    if technical.len() > Content::MAX_LAYER_CHARS {
        println!(
            "⚠️  Technical content truncated to {} chars.",
            Content::MAX_LAYER_CHARS
        );
        technical.truncate(Content::MAX_LAYER_CHARS);
    }

    println!("Principle content (optional, end with empty line):");
    io::stdout().flush()?;
    let principle_text = crate::read_multiline()?;
    let principle = if principle_text.is_empty() {
        None
    } else {
        let mut p = principle_text;
        if p.len() > Content::MAX_LAYER_CHARS {
            println!(
                "⚠️  Principle content truncated to {} chars.",
                Content::MAX_LAYER_CHARS
            );
            p.truncate(Content::MAX_LAYER_CHARS);
        }
        Some(p)
    };

    print!("Tags (comma-separated, e.g. swift,testing): ");
    io::stdout().flush()?;
    let mut tags_input = String::new();
    io::stdin().read_line(&mut tags_input)?;
    let topic_tags: Vec<String> = tags_input
        .trim()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    print!("Tier (session/project/core) [session]: ");
    io::stdout().flush()?;
    let mut tier_input = String::new();
    io::stdin().read_line(&mut tier_input)?;
    let tier = match tier_input.trim() {
        "project" => Tier::Project,
        "core" => Tier::Core,
        _ => Tier::Session,
    };

    // Handle --diagram flag
    let mut attachments = Vec::new();
    if let Some(ref diagram_file) = diagram_path {
        let source = std::path::Path::new(diagram_file);
        if !source.exists() {
            println!("❌ Diagram file not found: {}", diagram_file);
            return Ok(());
        }

        // Copy to assets dir and detect format
        let (relative_path, format) = store.copy_diagram_to_assets(&name, source)?;
        let att_type = AttachmentType::from_format(&format);

        // Prompt for description
        print!("Diagram description: ");
        io::stdout().flush()?;
        let mut diagram_desc = String::new();
        io::stdin().read_line(&mut diagram_desc)?;
        let diagram_desc = diagram_desc.trim().to_string();

        attachments.push(Attachment {
            att_type,
            format: format.clone(),
            path: relative_path.clone(),
            description: diagram_desc,
        });

        println!(
            "📎 Attached {} diagram: {}",
            format.fence_lang(),
            relative_path
        );
    }

    let pattern = Pattern {
        base: KnowledgeBase {
            schema: SCHEMA_VERSION,
            name: name.clone(),
            description: desc,
            content: Content::DualLayer {
                technical,
                principle,
            },
            tier,
            importance: 0.5,
            confidence: 0.9, // manually created = high confidence
            tags: Tags {
                languages: vec![],
                topics: topic_tags,
                extra: Default::default(),
            },
            applies: Applies::default(),
            evidence: Evidence::default(),
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments,
    };

    store.save(&pattern)?;
    println!("✅ Created pattern: {}", name);

    // Auto-discover links to existing patterns
    let existing = store.list_all()?;
    let suggestions = evolve::linker::discover_links(&pattern, &existing);
    if !suggestions.is_empty() {
        let mut pattern = store.get(&name)?;
        for s in &suggestions {
            match s.link_type {
                evolve::linker::LinkType::Related => {
                    if !pattern.links.related.contains(&s.target_name) {
                        pattern.links.related.push(s.target_name.clone());
                    }
                    // Bidirectional
                    if let Ok(mut target) = store.get(&s.target_name)
                        && !target.links.related.contains(&name)
                    {
                        target.links.related.push(name.clone());
                        let _ = store.save(&target);
                    }
                }
                evolve::linker::LinkType::Supersedes => {
                    if !pattern.links.supersedes.contains(&s.target_name) {
                        pattern.links.supersedes.push(s.target_name.clone());
                    }
                }
            }
            println!("  🔗 Linked to: {} ({:?})", s.target_name, s.link_type);
        }
        store.save(&pattern)?;
    }

    Ok(())
}

pub(crate) fn cmd_search(query: &str) -> Result<()> {
    use crate::retrieve::gate::{GateDecision, evaluate_query};
    use crate::retrieve::scoring::score_and_rank;

    // Adaptive gate
    match evaluate_query(query) {
        GateDecision::Skip(reason) => {
            println!("⏭️  Query skipped by gate: {}", reason);
            return Ok(());
        }
        GateDecision::Force => {
            println!("⚡ Force retrieval triggered");
        }
        GateDecision::Pass => {}
    }

    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;
    let results = score_and_rank(query, patterns);

    if results.is_empty() {
        println!("No patterns found for: {}", query);
        return Ok(());
    }

    println!("🔍 Found {} patterns for \"{}\":\n", results.len(), query);
    for sp in &results {
        let p = &sp.pattern;
        let tier_icon = match p.tier {
            Tier::Session => "📝",
            Tier::Project => "📁",
            Tier::Core => "⭐",
        };
        println!(
            "  {} {} (score: {:.3}, relevance: {:.3}, importance: {:.0}%)\n    {}",
            tier_icon,
            p.name,
            sp.score,
            sp.relevance,
            p.importance * 100.0,
            p.description
        );
    }

    Ok(())
}

pub(crate) fn cmd_pattern_show(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let p = store.get(name)?;

    println!("📋 Pattern: {}\n", p.name);
    println!("Description: {}", p.description);
    println!(
        "Tier: {:?} | Maturity: {:?} | Status: {:?}",
        p.tier, p.maturity, p.lifecycle.status
    );
    println!(
        "Importance: {:.0}% | Confidence: {:.0}%",
        p.importance * 100.0,
        p.confidence * 100.0
    );

    println!("\nContent:");
    match &p.content {
        Content::DualLayer {
            technical,
            principle,
        } => {
            println!("  Technical: {}", technical);
            if let Some(pr) = principle {
                println!("  Principle: {}", pr);
            }
        }
        Content::Plain(s) => println!("  {}", s),
    }

    if !p.tags.topics.is_empty() {
        println!("\nTags: {}", p.tags.topics.join(", "));
    }

    if p.evidence.injection_count > 0 {
        println!(
            "\nEvidence: {} injections, {:.0}% effectiveness",
            p.evidence.injection_count,
            p.evidence.effectiveness() * 100.0,
        );
    }

    // Show attachments
    if !p.attachments.is_empty() {
        println!("\nAttachments ({}):", p.attachments.len());
        for att in &p.attachments {
            println!(
                "  📎 [{:?}] {} — {}",
                att.att_type, att.path, att.description
            );
            // For text-based diagrams, print content inline
            if att.format.is_text_based() {
                if let Some(content) = store.resolve_attachment_content(att) {
                    println!("  ```{}", att.format.fence_lang());
                    for line in content.lines() {
                        println!("  {}", line);
                    }
                    println!("  ```");
                } else {
                    println!("  (file not found: {})", att.path);
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn cmd_edit(name: &str, quick: bool) -> Result<()> {
    let store = YamlStore::default_store()?;
    let old_pattern = store.get(name)?;

    // Show preview
    interactive::show_edit_preview(&old_pattern);

    if quick {
        // Quick inline edit
        use dialoguer::Select;

        let fields = &["description", "confidence", "importance", "tier", "tags"];
        let field_idx = Select::new()
            .with_prompt("  Which field?")
            .items(fields)
            .default(0)
            .interact()?;

        let mut pattern = old_pattern.clone();

        match fields[field_idx] {
            "description" => {
                let new_val: String = dialoguer::Input::new()
                    .with_prompt("  New description")
                    .default(pattern.description.clone())
                    .interact_text()?;
                pattern.description = new_val;
            }
            "confidence" => {
                let new_val: String = dialoguer::Input::new()
                    .with_prompt(format!(
                        "  New confidence (current: {:.2})",
                        pattern.confidence
                    ))
                    .default(format!("{:.2}", pattern.confidence))
                    .interact_text()?;
                pattern.confidence = new_val
                    .parse()
                    .unwrap_or(pattern.confidence)
                    .clamp(0.0, 1.0);
            }
            "importance" => {
                let new_val: String = dialoguer::Input::new()
                    .with_prompt(format!(
                        "  New importance (current: {:.2})",
                        pattern.importance
                    ))
                    .default(format!("{:.2}", pattern.importance))
                    .interact_text()?;
                pattern.importance = new_val
                    .parse()
                    .unwrap_or(pattern.importance)
                    .clamp(0.0, 1.0);
            }
            "tier" => {
                let tier_options = &["session", "project", "core"];
                let tier_idx = Select::new()
                    .with_prompt("  Select tier")
                    .items(tier_options)
                    .default(match pattern.tier {
                        Tier::Session => 0,
                        Tier::Project => 1,
                        Tier::Core => 2,
                    })
                    .interact()?;
                pattern.tier = match tier_idx {
                    1 => Tier::Project,
                    2 => Tier::Core,
                    _ => Tier::Session,
                };
            }
            "tags" => {
                let current = pattern.tags.topics.join(", ");
                let new_val: String = dialoguer::Input::new()
                    .with_prompt("  Tags (comma-separated)")
                    .default(current)
                    .interact_text()?;
                pattern.tags.topics = new_val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => unreachable!(),
        }

        pattern.updated_at = chrono::Utc::now();
        interactive::show_edit_diff(&old_pattern, &pattern);

        let apply = dialoguer::Confirm::new()
            .with_prompt("  Apply changes?")
            .default(true)
            .interact()?;

        if apply {
            store.save(&pattern)?;
            println!("  {} Saved.", console::style("OK").green().bold());
        } else {
            println!("  Discarded.");
        }
    } else {
        // Full edit: open in $EDITOR
        let yaml_path = dirs::home_dir()
            .expect("no home dir")
            .join(".mur")
            .join("patterns")
            .join(format!("{}.yaml", name));

        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let status = std::process::Command::new(&editor)
            .arg(&yaml_path)
            .status()?;

        if !status.success() {
            println!("  Editor exited with error.");
            return Ok(());
        }

        // Reload and show diff
        let new_pattern = store.get(name)?;
        interactive::show_edit_diff(&old_pattern, &new_pattern);
    }

    Ok(())
}

pub(crate) fn cmd_promote(name: &str, tier_str: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let mut pattern = store.get(name)?;

    let new_tier = match tier_str {
        "project" => Tier::Project,
        "core" => Tier::Core,
        _ => {
            println!("❌ Invalid tier: {}. Use 'project' or 'core'.", tier_str);
            return Ok(());
        }
    };

    let old_tier = pattern.tier;
    pattern.tier = new_tier;
    pattern.updated_at = chrono::Utc::now();
    store.save(&pattern)?;

    println!("⬆️  Promoted '{}': {:?} → {:?}", name, old_tier, new_tier);
    Ok(())
}

pub(crate) fn cmd_deprecate(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let mut pattern = store.get(name)?;

    pattern.lifecycle.status = LifecycleStatus::Deprecated;
    pattern.updated_at = chrono::Utc::now();
    store.save(&pattern)?;

    println!("⚠️  Deprecated '{}'", name);
    Ok(())
}

pub(crate) fn cmd_boost(name: &str, amount: f64) -> Result<()> {
    let store = YamlStore::default_store()?;
    let mut pattern = store.get(name)?;

    let old = pattern.importance;
    pattern.importance = (pattern.importance + amount).min(1.0);
    pattern.updated_at = chrono::Utc::now();
    store.save(&pattern)?;

    println!(
        "🚀 Boosted {}: {:.0}% → {:.0}%",
        name,
        old * 100.0,
        pattern.importance * 100.0
    );
    Ok(())
}

pub(crate) fn cmd_feedback(name: &str, helpful: bool) -> Result<()> {
    use crate::evolve::feedback::{FeedbackSignal, apply_feedback};

    let store = YamlStore::default_store()?;
    let mut pattern = store.get(name)?;

    let signal = if helpful {
        println!("👍 Recorded helpful feedback for {}", name);
        FeedbackSignal::Helpful
    } else {
        println!("👎 Recorded unhelpful feedback for {}", name);
        FeedbackSignal::Unhelpful
    };

    let old_importance = pattern.importance;
    apply_feedback(&mut pattern, signal);
    store.save(&pattern)?;

    let eff = pattern.evidence.effectiveness();
    println!(
        "   Effectiveness: {:.0}% ({} success / {} override)",
        eff * 100.0,
        pattern.evidence.success_signals,
        pattern.evidence.override_signals
    );
    println!(
        "   Importance: {:.0}% → {:.0}%",
        old_importance * 100.0,
        pattern.importance * 100.0
    );

    Ok(())
}

pub(crate) fn cmd_feedback_auto(file: Option<String>, dry_run: bool) -> Result<()> {
    use crate::capture::feedback::{SignalType, analyze_session_feedback, read_injection_record};
    use crate::evolve::feedback::{FeedbackSignal, apply_feedback};

    // 1. Read injection record
    let record = match read_injection_record() {
        Ok(r) => r,
        Err(e) => {
            println!(
                "❌ No injection record found (~/.mur/last_injection.json): {}",
                e
            );
            println!("   Run `mur inject` first, then analyze the session.");
            return Ok(());
        }
    };

    if record.patterns.is_empty() {
        println!("No patterns were injected in the last session.");
        return Ok(());
    }

    // 2. Read transcript
    let transcript = match file {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read transcript file '{}': {}", path, e))?,
        None => {
            // Read from stdin
            let mut buf = String::new();
            io::Read::read_to_string(&mut io::stdin(), &mut buf)?;
            buf
        }
    };

    if transcript.trim().is_empty() {
        println!("❌ Empty transcript. Provide a session transcript via --file or stdin.");
        return Ok(());
    }

    // 3. Analyze
    let results = analyze_session_feedback(&transcript, &record.patterns);

    // 4. Report and apply
    let store = YamlStore::default_store()?;
    let mode = if dry_run { " (dry run)" } else { "" };
    println!("🔍 Post-session feedback{}\n", mode);
    println!("   Injection: {} ({})", record.query, record.timestamp);
    println!("   Patterns analyzed: {}\n", results.len());

    for fb in &results {
        let (icon, label) = match fb.signal {
            SignalType::Reinforced => ("✅", "Reinforced"),
            SignalType::Contradicted => ("❌", "Contradicted"),
            SignalType::Ignored => ("⏭️ ", "Ignored"),
        };
        let delta = if fb.confidence_delta >= 0.0 {
            format!("+{:.2}", fb.confidence_delta)
        } else {
            format!("{:.2}", fb.confidence_delta)
        };
        print!("   {} {}: {} ({})", icon, fb.pattern_name, label, delta);
        if let Some(ev) = &fb.evidence {
            print!("\n      Evidence: \"{}\"", ev);
        }
        println!();

        if !dry_run {
            // Apply confidence delta via existing feedback system
            if let Ok(mut pattern) = store.get(&fb.pattern_name) {
                let signal = match fb.signal {
                    SignalType::Reinforced => FeedbackSignal::Success,
                    SignalType::Contradicted => FeedbackSignal::Unhelpful,
                    SignalType::Ignored => FeedbackSignal::Override,
                };
                // For auto feedback, apply the specific confidence delta
                // rather than the default feedback amounts
                let old_confidence = pattern.confidence;
                apply_feedback(&mut pattern, signal);
                // Override confidence with our computed delta instead
                pattern.confidence = (old_confidence + fb.confidence_delta).clamp(0.0, 1.0);
                let _ = store.save(&pattern);
            }
        }
    }

    let reinforced = results
        .iter()
        .filter(|r| r.signal == SignalType::Reinforced)
        .count();
    let contradicted = results
        .iter()
        .filter(|r| r.signal == SignalType::Contradicted)
        .count();
    let ignored = results
        .iter()
        .filter(|r| r.signal == SignalType::Ignored)
        .count();

    println!("\n── Summary{} ──", mode);
    println!("   Reinforced:   {}", reinforced);
    println!("   Contradicted: {}", contradicted);
    println!("   Ignored:      {}", ignored);

    Ok(())
}

pub(crate) fn cmd_set_lifecycle(name: &str, action: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let mut pattern = store.get(name)?;

    match action {
        "pin" => {
            pattern.lifecycle.pinned = !pattern.lifecycle.pinned;
            let state = if pattern.lifecycle.pinned {
                "pinned"
            } else {
                "unpinned"
            };
            store.save(&pattern)?;
            println!("📌 {} is now {}.", name, state);
        }
        "mute" => {
            pattern.lifecycle.muted = !pattern.lifecycle.muted;
            let state = if pattern.lifecycle.muted {
                "muted"
            } else {
                "unmuted"
            };
            store.save(&pattern)?;
            println!("🔇 {} is now {}.", name, state);
        }
        _ => unreachable!(),
    }

    Ok(())
}

pub(crate) fn cmd_links(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;

    println!("🔗 Links for '{}':\n", name);

    if !pattern.links.related.is_empty() {
        println!("  Related:");
        for r in &pattern.links.related {
            println!("    ↔ {}", r);
        }
    }
    if !pattern.links.supersedes.is_empty() {
        println!("  Supersedes:");
        for s in &pattern.links.supersedes {
            println!("    → {} (deprecated)", s);
        }
    }
    if !pattern.links.workflows.is_empty() {
        println!("  Workflows:");
        for w in &pattern.links.workflows {
            println!("    📋 {}", w);
        }
    }
    if pattern.links.related.is_empty()
        && pattern.links.supersedes.is_empty()
        && pattern.links.workflows.is_empty()
    {
        println!("  No links yet. Run `mur gc` to auto-discover links.");
    }

    Ok(())
}
