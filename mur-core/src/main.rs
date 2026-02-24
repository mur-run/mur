use anyhow::Result;
use clap::{Parser, Subcommand};
use mur_common::pattern::*;
use std::io::{self, Write};

mod capture;
mod evolve;
mod inject;
mod migrate;
mod retrieve;
mod store;

use store::yaml::YamlStore;

#[derive(Parser)]
#[command(name = "mur", version, about = "Continuous learning for AI assistants")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new pattern interactively
    New,
    /// Search patterns (keyword match)
    Search {
        /// Search query
        query: String,
    },
    /// Learn from sessions
    Learn {
        #[command(subcommand)]
        action: LearnAction,
    },
    /// Show statistics and effectiveness
    Stats,
    /// Sync patterns to AI tools
    Sync,
    /// Inject patterns for a query (hook integration)
    Inject {
        #[arg(long)]
        query: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Report pattern feedback
    Feedback {
        #[command(subcommand)]
        action: FeedbackAction,
    },
    /// Migrate v1 patterns to v2 schema
    Migrate,
    /// Garbage collect low-quality patterns
    Gc {
        /// Auto-archive without prompting
        #[arg(long)]
        auto: bool,
    },
    /// Pin a pattern (never auto-deprecated)
    Pin {
        /// Pattern name
        name: String,
    },
    /// Mute a pattern (skip injection)
    Mute {
        /// Pattern name
        name: String,
    },
    /// Boost a pattern's importance
    Boost {
        /// Pattern name
        name: String,
        /// Amount to boost (default: 0.1)
        #[arg(long, default_value = "0.1")]
        amount: f64,
    },
    /// Rebuild index from YAML files
    Reindex,
    /// Show pattern connections
    Links {
        /// Pattern name
        name: String,
    },
    /// Terminal dashboard
    Dashboard,
    /// Community publish/fetch
    Community {
        #[command(subcommand)]
        action: CommunityAction,
    },
}

#[derive(Subcommand)]
enum LearnAction {
    /// Extract patterns from a session transcript
    Extract {
        #[arg(short, long)]
        file: Option<String>,
    },
}

#[derive(Subcommand)]
enum FeedbackAction {
    /// Mark a pattern as helpful
    Helpful { name: String },
    /// Mark a pattern as unhelpful
    Unhelpful { name: String },
}

#[derive(Subcommand)]
enum CommunityAction {
    /// Publish a pattern to mur.run
    Publish { name: String },
    /// Fetch a pattern from mur.run
    Fetch { name: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::New => cmd_new()?,
        Commands::Search { query } => cmd_search(&query)?,
        Commands::Stats => cmd_stats()?,
        Commands::Pin { name } => cmd_set_lifecycle(&name, "pin")?,
        Commands::Mute { name } => cmd_set_lifecycle(&name, "mute")?,
        Commands::Boost { name, amount } => cmd_boost(&name, amount)?,
        Commands::Feedback { action } => match action {
            FeedbackAction::Helpful { name } => cmd_feedback(&name, true)?,
            FeedbackAction::Unhelpful { name } => cmd_feedback(&name, false)?,
        },
        Commands::Gc { auto } => cmd_gc(auto)?,
        Commands::Migrate => cmd_migrate()?,
        Commands::Learn { action } => match action {
            LearnAction::Extract { file: _ } => {
                println!("📚 Extract requires LLM integration (Phase 1, Week 3)");
                todo!()
            }
        },
        Commands::Sync => {
            println!("🔄 Smart sync (Phase 2)");
            todo!()
        }
        Commands::Inject {
            query: _,
            project: _,
        } => {
            println!("💉 Smart inject (Phase 2)");
            todo!()
        }
        Commands::Reindex => {
            println!("🔄 Reindex (Phase 2 — needs LanceDB)");
            todo!()
        }
        Commands::Links { name: _ } => {
            println!("🔗 Links (Phase 3)");
            todo!()
        }
        Commands::Dashboard => {
            println!("📊 Dashboard (Phase 2)");
            todo!()
        }
        Commands::Community { action: _ } => {
            println!("🌐 Community (Phase 4)");
            todo!()
        }
    }

    Ok(())
}

// ─── Command implementations ───────────────────────────────────────

fn cmd_new() -> Result<()> {
    let store = YamlStore::default_store()?;

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
    let technical = read_multiline()?;

    println!("Principle content (optional, end with empty line):");
    io::stdout().flush()?;
    let principle_text = read_multiline()?;
    let principle = if principle_text.is_empty() {
        None
    } else {
        Some(principle_text)
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

    let pattern = Pattern {
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
    };

    store.save(&pattern)?;
    println!("✅ Created pattern: {}", name);
    Ok(())
}

fn cmd_search(query: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    let query_lower = query.to_lowercase();
    let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

    let mut scored: Vec<(&Pattern, f64)> = patterns
        .iter()
        .filter_map(|p| {
            let text = format!(
                "{} {} {} {}",
                p.name,
                p.description,
                p.content.as_text(),
                p.tags.topics.join(" ")
            )
            .to_lowercase();

            let mut score = 0.0;
            for term in &query_terms {
                if text.contains(term) {
                    score += 1.0;
                }
                // Bonus for name match
                if p.name.to_lowercase().contains(term) {
                    score += 0.5;
                }
            }

            if score > 0.0 {
                // Weight by importance
                score *= 0.7 + 0.3 * p.importance;
                Some((p, score))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    if scored.is_empty() {
        println!("No patterns found for: {}", query);
        return Ok(());
    }

    println!("🔍 Found {} patterns for \"{}\":\n", scored.len(), query);
    for (p, score) in scored.iter().take(10) {
        let tier_icon = match p.tier {
            Tier::Session => "📝",
            Tier::Project => "📁",
            Tier::Core => "⭐",
        };
        let status = match p.lifecycle.status {
            LifecycleStatus::Active => "",
            LifecycleStatus::Deprecated => " [deprecated]",
            LifecycleStatus::Archived => " [archived]",
        };
        println!(
            "  {} {} (score: {:.1}, importance: {:.0}%){}\n    {}",
            tier_icon,
            p.name,
            score,
            p.importance * 100.0,
            status,
            p.description
        );
    }

    Ok(())
}

fn cmd_stats() -> Result<()> {
    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    let total = patterns.len();
    let mut session_count = 0;
    let mut project_count = 0;
    let mut core_count = 0;
    let mut active_count = 0;
    let mut deprecated_count = 0;
    let mut archived_count = 0;
    let mut total_importance = 0.0;
    let mut total_effectiveness = 0.0;
    let mut tracked_count = 0u64;
    let mut total_injections = 0u64;

    for p in &patterns {
        match p.tier {
            Tier::Session => session_count += 1,
            Tier::Project => project_count += 1,
            Tier::Core => core_count += 1,
        }
        match p.lifecycle.status {
            LifecycleStatus::Active => active_count += 1,
            LifecycleStatus::Deprecated => deprecated_count += 1,
            LifecycleStatus::Archived => archived_count += 1,
        }
        total_importance += p.importance;
        total_injections += p.evidence.injection_count;
        if p.evidence.injection_count > 0 {
            tracked_count += 1;
            total_effectiveness += p.evidence.effectiveness();
        }
    }

    let avg_importance = if total > 0 {
        total_importance / total as f64
    } else {
        0.0
    };
    let avg_effectiveness = if tracked_count > 0 {
        total_effectiveness / tracked_count as f64
    } else {
        0.0
    };

    println!("📊 MUR Core v2 Statistics");
    println!("─────────────────────────");
    println!("Total patterns:     {}", total);
    println!();
    println!("By tier:");
    println!("  📝 Session:       {}", session_count);
    println!("  📁 Project:       {}", project_count);
    println!("  ⭐ Core:          {}", core_count);
    println!();
    println!("By status:");
    println!("  ✅ Active:        {}", active_count);
    println!("  ⚠️  Deprecated:    {}", deprecated_count);
    println!("  📦 Archived:      {}", archived_count);
    println!();
    println!("Avg importance:     {:.0}%", avg_importance * 100.0);
    println!("Total injections:   {}", total_injections);
    println!(
        "Tracked patterns:   {} / {} ({:.0}%)",
        tracked_count,
        total,
        if total > 0 {
            tracked_count as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    );
    println!("Avg effectiveness:  {:.0}%", avg_effectiveness * 100.0);

    Ok(())
}

fn cmd_set_lifecycle(name: &str, action: &str) -> Result<()> {
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

fn cmd_boost(name: &str, amount: f64) -> Result<()> {
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

fn cmd_feedback(name: &str, helpful: bool) -> Result<()> {
    let store = YamlStore::default_store()?;
    let mut pattern = store.get(name)?;

    if helpful {
        pattern.evidence.success_signals += 1;
        println!("👍 Recorded helpful feedback for {}", name);
    } else {
        pattern.evidence.override_signals += 1;
        println!("👎 Recorded unhelpful feedback for {}", name);
    }

    let eff = pattern.evidence.effectiveness();
    pattern.updated_at = chrono::Utc::now();
    store.save(&pattern)?;

    println!(
        "   Effectiveness: {:.0}% ({} success / {} override)",
        eff * 100.0,
        pattern.evidence.success_signals,
        pattern.evidence.override_signals
    );

    Ok(())
}

fn cmd_gc(auto: bool) -> Result<()> {
    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    let candidates: Vec<&Pattern> = patterns
        .iter()
        .filter(|p| {
            // Skip pinned
            if p.lifecycle.pinned {
                return false;
            }
            // Low confidence
            if p.confidence < 0.5 {
                return true;
            }
            // Low effectiveness with enough data
            let total = p.evidence.success_signals + p.evidence.override_signals;
            if total >= 5 && p.evidence.effectiveness() < 0.2 {
                return true;
            }
            false
        })
        .collect();

    if candidates.is_empty() {
        println!("✨ No patterns need cleanup.");
        return Ok(());
    }

    println!(
        "🧹 Found {} patterns for cleanup:\n",
        candidates.len()
    );
    for p in &candidates {
        let reason = if p.confidence < 0.5 {
            format!("low confidence ({:.0}%)", p.confidence * 100.0)
        } else {
            format!("low effectiveness ({:.0}%)", p.evidence.effectiveness() * 100.0)
        };
        println!("  {} — {}", p.name, reason);
    }

    if auto {
        println!();
        let mut archived = 0;
        for p in &candidates {
            if store.archive(&p.name)? {
                archived += 1;
            }
        }
        println!("📦 Archived {} patterns.", archived);
    } else {
        println!("\nRun `mur gc --auto` to archive these patterns.");
    }

    Ok(())
}

fn cmd_migrate() -> Result<()> {
    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    let mut migrated = 0;
    let mut already_v2 = 0;

    for mut p in patterns {
        if p.schema >= 2 {
            already_v2 += 1;
            continue;
        }

        // Migrate v1 → v2
        p.schema = SCHEMA_VERSION;

        // Convert plain content to dual-layer
        if let Content::Plain(text) = &p.content {
            p.content = Content::DualLayer {
                technical: text.clone(),
                principle: None,
            };
        }

        // Set defaults for new fields
        if p.tier == Tier::Session && p.importance == 0.5 {
            // Already default, just ensure schema is set
        }

        p.updated_at = chrono::Utc::now();
        store.save(&p)?;
        migrated += 1;
    }

    println!("🔄 Migration complete:");
    println!("   Migrated:    {}", migrated);
    println!("   Already v2:  {}", already_v2);

    Ok(())
}

fn read_multiline() -> Result<String> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        lines.push(line);
    }
    Ok(lines.join("").trim_end().to_string())
}
