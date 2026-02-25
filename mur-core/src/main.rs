use anyhow::Result;
use clap::{Parser, Subcommand};
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::io::{self, Write};

mod capture;
mod evolve;
mod inject;
mod migrate;
mod retrieve;
mod store;

use store::yaml::YamlStore;
use store::workflow_yaml::WorkflowYamlStore;

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
    /// Promote a pattern's tier
    Promote {
        /// Pattern name
        name: String,
        /// Target tier (project/core)
        #[arg(long, default_value = "project")]
        tier: String,
    },
    /// Deprecate a pattern manually
    Deprecate {
        /// Pattern name
        name: String,
    },
    /// Manage workflows
    Workflow {
        #[command(subcommand)]
        action: WorkflowAction,
    },
    /// Rebuild index from YAML files
    Reindex,
    /// Show pattern connections
    Links {
        /// Pattern name
        name: String,
    },
    /// Run decay + maturity evaluation
    Evolve {
        /// Preview changes without saving
        #[arg(long)]
        dry_run: bool,
        /// Run even if recently evolved
        #[arg(long)]
        force: bool,
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
    /// Auto-analyze session transcript against injected patterns
    Auto {
        /// Path to session transcript (reads stdin if omitted)
        #[arg(long)]
        file: Option<String>,
        /// Preview changes without saving
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum WorkflowAction {
    /// List all workflows
    List,
    /// Show a workflow by name
    Show { name: String },
    /// Create a new workflow interactively
    New,
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
            FeedbackAction::Auto { file, dry_run } => cmd_feedback_auto(file, dry_run)?,
        },
        Commands::Gc { auto } => cmd_gc(auto)?,
        Commands::Migrate => cmd_migrate()?,
        Commands::Learn { action } => match action {
            LearnAction::Extract { file: _ } => {
                println!("📚 Extract requires LLM integration (Phase 1, Week 3)");
                todo!()
            }
        },
        Commands::Sync => cmd_sync()?,
        Commands::Inject {
            query,
            project: _,
        } => cmd_inject(&query).await?,
        Commands::Workflow { action } => match action {
            WorkflowAction::List => cmd_workflow_list()?,
            WorkflowAction::Show { name } => cmd_workflow_show(&name)?,
            WorkflowAction::New => cmd_workflow_new()?,
        },
        Commands::Reindex => cmd_reindex().await?,
        Commands::Promote { name, tier } => cmd_promote(&name, &tier)?,
        Commands::Deprecate { name } => cmd_deprecate(&name)?,
        Commands::Links { name } => cmd_links(&name)?,
        Commands::Evolve { dry_run, force } => cmd_evolve(dry_run, force)?,
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
    let mut technical = read_multiline()?;
    if technical.len() > Content::MAX_LAYER_CHARS {
        println!("⚠️  Technical content truncated to {} chars.", Content::MAX_LAYER_CHARS);
        technical.truncate(Content::MAX_LAYER_CHARS);
    }

    println!("Principle content (optional, end with empty line):");
    io::stdout().flush()?;
    let principle_text = read_multiline()?;
    let principle = if principle_text.is_empty() {
        None
    } else {
        let mut p = principle_text;
        if p.len() > Content::MAX_LAYER_CHARS {
            println!("⚠️  Principle content truncated to {} chars.", Content::MAX_LAYER_CHARS);
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
        attachments: vec![],
    };

    store.save(&pattern)?;
    println!("✅ Created pattern: {}", name);
    Ok(())
}

fn cmd_search(query: &str) -> Result<()> {
    use retrieve::gate::{evaluate_query, GateDecision};
    use retrieve::scoring::score_and_rank;

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

async fn cmd_inject(query: &str) -> Result<()> {
    use inject::hook::{detect_trigger, format_unified_injection, HookTrigger};
    use retrieve::gate::{evaluate_query, GateDecision};
    use retrieve::scoring::{score_and_rank, score_and_rank_hybrid};
    use store::embedding::{embed, EmbeddingConfig};
    use store::lancedb::VectorStore;
    use std::collections::HashMap;

    // Detect trigger type
    let trigger = detect_trigger(query);
    match &trigger {
        HookTrigger::OnError => eprintln!("# Trigger: OnError — searching for error-related patterns"),
        HookTrigger::OnRetry => eprintln!("# Trigger: OnRetry — searching for previous solutions"),
        _ => {}
    }

    if let GateDecision::Skip(reason) = evaluate_query(query) {
        eprintln!("# No patterns (gate: {})", reason);
        return Ok(());
    }

    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;

    // Also load workflows
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    // Try hybrid search if LanceDB index exists
    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

    let results = if index_path.exists() {
        // Try vector search
        let cfg = store::config::load_config()?;
        let config = EmbeddingConfig::from_config(&cfg);
        match embed(query, &config).await {
            Ok(query_embedding) => {
                let vector_store = VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
                let vector_results = vector_store.search(&query_embedding, 20, None).await?;
                let vector_scores: HashMap<String, f64> = vector_results
                    .into_iter()
                    .map(|r| (r.name, r.similarity as f64))
                    .collect();
                score_and_rank_hybrid(query, patterns, &vector_scores)
            }
            Err(_) => {
                // Embedding failed (Ollama not running?), fall back to keyword
                eprintln!("# Falling back to keyword search (embedding unavailable)");
                score_and_rank(query, patterns)
            }
        }
    } else {
        score_and_rank(query, patterns)
    };

    // Filter out archived patterns, touch timestamps for injected ones
    let mut injected_patterns: Vec<Pattern> = Vec::new();
    for sp in results {
        let mut p = sp.pattern;
        if p.lifecycle.status == LifecycleStatus::Archived {
            continue;
        }
        // Touch timestamps on injection
        let now = chrono::Utc::now();
        p.decay.last_active = Some(now);
        p.evidence.injection_count += 1;
        p.lifecycle.last_injected = Some(now);
        p.updated_at = now;
        // Save touched pattern (best-effort, don't fail injection on save error)
        let _ = yaml_store.save(&p);
        injected_patterns.push(p);
    }

    let output = format_unified_injection(&injected_patterns, &workflows, 2000);

    if output.is_empty() {
        eprintln!("# No relevant patterns found");
    } else {
        // Record what was injected for post-session feedback analysis
        let project_name = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        inject::hook::record_injection(query, &project_name, &injected_patterns);

        print!("{}", output);
    }

    Ok(())
}

fn cmd_stats() -> Result<()> {
    use mur_common::knowledge::Maturity;

    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    let total = patterns.len();
    let mut session_count = 0;
    let mut project_count = 0;
    let mut core_count = 0;
    let mut active_count = 0;
    let mut deprecated_count = 0;
    let mut archived_count = 0;
    let mut draft_count = 0;
    let mut emerging_count = 0;
    let mut stable_count = 0;
    let mut canonical_count = 0;
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
        match p.maturity {
            Maturity::Draft => draft_count += 1,
            Maturity::Emerging => emerging_count += 1,
            Maturity::Stable => stable_count += 1,
            Maturity::Canonical => canonical_count += 1,
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
    println!(
        "By maturity:        Draft: {} | Emerging: {} | Stable: {} | Canonical: {}",
        draft_count, emerging_count, stable_count, canonical_count
    );
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
    use evolve::feedback::{apply_feedback, FeedbackSignal};

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

fn cmd_feedback_auto(file: Option<String>, dry_run: bool) -> Result<()> {
    use capture::feedback::{analyze_session_feedback, read_injection_record, SignalType};
    use evolve::feedback::{apply_feedback, FeedbackSignal};

    // 1. Read injection record
    let record = match read_injection_record() {
        Ok(r) => r,
        Err(e) => {
            println!("❌ No injection record found (~/.mur/last_injection.json): {}", e);
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

    let reinforced = results.iter().filter(|r| r.signal == SignalType::Reinforced).count();
    let contradicted = results.iter().filter(|r| r.signal == SignalType::Contradicted).count();
    let ignored = results.iter().filter(|r| r.signal == SignalType::Ignored).count();

    println!("\n── Summary{} ──", mode);
    println!("   Reinforced:   {}", reinforced);
    println!("   Contradicted: {}", contradicted);
    println!("   Ignored:      {}", ignored);

    Ok(())
}

fn cmd_gc(auto: bool) -> Result<()> {
    use evolve::decay::apply_decay_all;
    use evolve::maturity::apply_maturity_all;
    use evolve::lifecycle::{evaluate_lifecycle, apply_lifecycle_action, LifecycleAction};
    use mur_common::knowledge::Maturity;

    let store = YamlStore::default_store()?;
    let now = chrono::Utc::now();

    // Run decay + maturity before lifecycle evaluation
    let decay_report = apply_decay_all(&store, now)?;
    if decay_report.patterns_decayed > 0 {
        println!(
            "📉 Decayed {} patterns ({} auto-archived).",
            decay_report.patterns_decayed, decay_report.patterns_archived
        );
    }

    let maturity_report = apply_maturity_all(&store, now)?;
    if maturity_report.promotions + maturity_report.demotions > 0 {
        println!(
            "🎯 Maturity: {} promotions, {} demotions.",
            maturity_report.promotions, maturity_report.demotions
        );
    }

    let patterns = store.list_all()?;

    // First pass: apply lifecycle evaluations (deprecate/archive)
    let mut lifecycle_changes = 0;
    for mut p in patterns.clone() {
        let action = evaluate_lifecycle(&p);
        if action != LifecycleAction::None {
            let desc = match &action {
                LifecycleAction::Promote(tier) => format!("promoted to {:?}", tier),
                LifecycleAction::Deprecate => "deprecated".into(),
                LifecycleAction::Archive => "archived".into(),
                LifecycleAction::None => unreachable!(),
            };
            println!("  🔄 {} — {}", p.name, desc);
            apply_lifecycle_action(&mut p, &action);
            store.save(&p)?;
            lifecycle_changes += 1;
        }
    }
    if lifecycle_changes > 0 {
        println!("Applied {} lifecycle changes.\n", lifecycle_changes);
    }

    // Link discovery pass
    {
        use evolve::linker::{discover_links, LinkType};
        let all = store.list_all()?;

        // Phase 1: Collect all link suggestions (read-only)
        let mut pairs: Vec<(String, String, LinkType)> = Vec::new();
        for pattern in &all {
            for s in discover_links(pattern, &all) {
                pairs.push((pattern.name.clone(), s.target_name, s.link_type));
            }
        }

        if !pairs.is_empty() {
            // Phase 2: Apply links
            let mut by_name: std::collections::HashMap<String, Pattern> =
                all.into_iter().map(|p| (p.name.clone(), p)).collect();
            let mut changed = std::collections::HashSet::new();
            let mut link_count = 0usize;

            for (source, target, link_type) in &pairs {
                match link_type {
                    LinkType::Related => {
                        if let Some(p) = by_name.get_mut(source)
                            && !p.links.related.contains(target)
                        {
                            p.links.related.push(target.clone());
                            changed.insert(source.clone());
                            link_count += 1;
                        }
                        if let Some(p) = by_name.get_mut(target)
                            && !p.links.related.contains(source)
                        {
                            p.links.related.push(source.clone());
                            changed.insert(target.clone());
                            link_count += 1;
                        }
                    }
                    LinkType::Supersedes => {
                        if let Some(p) = by_name.get_mut(source)
                            && !p.links.supersedes.contains(target)
                        {
                            p.links.supersedes.push(target.clone());
                            changed.insert(source.clone());
                            link_count += 1;
                        }
                    }
                }
            }

            // Phase 3: Save changed patterns
            for name in &changed {
                if let Some(p) = by_name.get(name) {
                    store.save(p)?;
                }
            }

            if link_count > 0 {
                println!("🔗 Discovered {} new links.\n", link_count);
            }
        }
    }

    // Second pass: find low-quality candidates for archival
    let patterns = store.list_all()?; // reload after changes
    let candidates: Vec<&Pattern> = patterns
        .iter()
        .filter(|p| {
            if p.lifecycle.pinned || p.lifecycle.status != LifecycleStatus::Active {
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

    // Show maturity distribution
    let all = store.list_all()?;
    let (mut draft, mut emerging, mut stable, mut canonical) = (0usize, 0, 0, 0);
    for p in &all {
        match p.maturity {
            Maturity::Draft => draft += 1,
            Maturity::Emerging => emerging += 1,
            Maturity::Stable => stable += 1,
            Maturity::Canonical => canonical += 1,
        }
    }
    println!(
        "\nMaturity: Draft: {} | Emerging: {} | Stable: {} | Canonical: {}",
        draft, emerging, stable, canonical
    );

    Ok(())
}

fn cmd_migrate() -> Result<()> {
    use migrate::migrate_directory;

    let mur_dir = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("patterns");

    println!("🔄 Migrating patterns in {}...", mur_dir.display());

    let result = migrate_directory(&mur_dir)?;

    println!("✅ Migration complete:");
    println!("   Migrated:    {}", result.migrated);
    println!("   Already v2:  {}", result.already_v2);
    println!("   Skipped:     {}", result.skipped);

    if !result.errors.is_empty() {
        println!("\n⚠️  Issues:");
        for e in &result.errors {
            println!("   {}", e);
        }
    }

    Ok(())
}

fn cmd_promote(name: &str, tier_str: &str) -> Result<()> {
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

    println!(
        "⬆️  Promoted '{}': {:?} → {:?}",
        name, old_tier, new_tier
    );
    Ok(())
}

fn cmd_deprecate(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let mut pattern = store.get(name)?;

    pattern.lifecycle.status = LifecycleStatus::Deprecated;
    pattern.updated_at = chrono::Utc::now();
    store.save(&pattern)?;

    println!("⚠️  Deprecated '{}'", name);
    Ok(())
}

fn cmd_links(name: &str) -> Result<()> {
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

fn cmd_sync() -> Result<()> {
    use evolve::decay::apply_decay_all;
    use evolve::maturity::apply_maturity_all;
    use inject::sync::{default_targets, generate_sync_content, write_sync_file};
    use retrieve::scoring::score_and_rank;

    let store = YamlStore::default_store()?;
    let now = chrono::Utc::now();

    // Run decay + maturity before syncing
    let _ = apply_decay_all(&store, now)?;
    let _ = apply_maturity_all(&store, now)?;

    let patterns = store.list_all()?;

    if patterns.is_empty() {
        println!("No patterns to sync.");
        return Ok(());
    }

    // Get current working directory for project-scoped sync
    let cwd = std::env::current_dir()?;
    let targets = default_targets();

    for target in &targets {
        let target_path = cwd.join(&target.file);

        // Only write to files that already exist on disk
        if !target_path.exists() {
            continue;
        }

        // Score patterns for this project context
        let project_name = cwd
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Use the project name as a rough query for relevance
        let scored = score_and_rank(&project_name, patterns.clone());
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
        println!(
            "  ✅ {} — wrote {} patterns to {}",
            target.name,
            top.len(),
            target_path.display()
        );
    }

    println!("🔄 Sync complete.");
    Ok(())
}

async fn cmd_reindex() -> Result<()> {
    use store::embedding::{embed, EmbeddingConfig};
    use store::lancedb::VectorStore;

    let pattern_store = YamlStore::default_store()?;
    let patterns = pattern_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    if patterns.is_empty() && workflows.is_empty() {
        println!("No patterns or workflows to index.");
        return Ok(());
    }

    let cfg = store::config::load_config()?;
    let config = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

    println!(
        "🔄 Reindexing {} patterns + {} workflows using {} ({})...",
        patterns.len(),
        workflows.len(),
        config.model,
        match &config.provider {
            store::embedding::EmbeddingProvider::Ollama { base_url } => base_url.clone(),
            store::embedding::EmbeddingProvider::OpenAI { .. } => "OpenAI".into(),
        }
    );

    let mut indexed_patterns = Vec::new();
    let mut indexed_workflows = Vec::new();
    let mut errors = 0;
    let total = patterns.len() + workflows.len();

    for (i, pattern) in patterns.iter().enumerate() {
        let text = format!("{}: {}\n{}", pattern.name, pattern.description, pattern.content.as_text());
        match embed(&text, &config).await {
            Ok(embedding) => {
                indexed_patterns.push((pattern.clone(), embedding));
                if (i + 1) % 10 == 0 {
                    println!("  {}/{} embedded...", i + 1, total);
                }
            }
            Err(e) => {
                eprintln!("  ⚠️  {} — {}", pattern.name, e);
                errors += 1;
            }
        }
    }

    for (i, workflow) in workflows.iter().enumerate() {
        let text = format!("{}: {}\n{}", workflow.name, workflow.description, workflow.content.as_text());
        match embed(&text, &config).await {
            Ok(embedding) => {
                indexed_workflows.push((workflow.clone(), embedding));
                let idx = patterns.len() + i + 1;
                if idx % 10 == 0 {
                    println!("  {}/{} embedded...", idx, total);
                }
            }
            Err(e) => {
                eprintln!("  ⚠️  {} — {}", workflow.name, e);
                errors += 1;
            }
        }
    }

    let vector_store = VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
    vector_store.build_unified_index(&indexed_patterns, &indexed_workflows).await?;

    println!(
        "✅ Indexed {} patterns + {} workflows ({} errors). Index: {}",
        indexed_patterns.len(),
        indexed_workflows.len(),
        errors,
        index_path.display()
    );

    Ok(())
}

fn cmd_workflow_list() -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let workflows = store.list_all()?;

    if workflows.is_empty() {
        println!("No workflows found. Create one with `mur workflow new`.");
        return Ok(());
    }

    println!("📋 Workflows ({}):\n", workflows.len());
    for w in &workflows {
        let steps = w.steps.len();
        println!("  {} — {} ({} steps)", w.name, w.description, steps);
    }

    Ok(())
}

fn cmd_workflow_show(name: &str) -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let w = store.get(name)?;

    println!("📋 Workflow: {}\n", w.name);
    println!("Description: {}", w.description);
    println!("Content: {}", w.content.as_text());

    if !w.steps.is_empty() {
        println!("\nSteps:");
        for step in &w.steps {
            print!("  {}. {}", step.order, step.description);
            if let Some(cmd) = &step.command {
                print!(" (`{}`)", cmd);
            }
            println!();
        }
    }

    if !w.tools.is_empty() {
        println!("\nTools: {}", w.tools.join(", "));
    }

    if !w.trigger.is_empty() {
        println!("Trigger: {}", w.trigger);
    }

    Ok(())
}

fn cmd_workflow_new() -> Result<()> {
    use mur_common::workflow::{Step, FailureAction};

    let store = WorkflowYamlStore::default_store()?;

    print!("Workflow name (kebab-case): ");
    io::stdout().flush()?;
    let mut name = String::new();
    io::stdin().read_line(&mut name)?;
    let name = name.trim().to_string();

    if name.is_empty() {
        println!("Name cannot be empty.");
        return Ok(());
    }
    if store.exists(&name) {
        println!("Workflow '{}' already exists.", name);
        return Ok(());
    }

    print!("Description: ");
    io::stdout().flush()?;
    let mut desc = String::new();
    io::stdin().read_line(&mut desc)?;
    let desc = desc.trim().to_string();

    print!("Trigger (when to use, e.g. 'when deploying to production'): ");
    io::stdout().flush()?;
    let mut trigger = String::new();
    io::stdin().read_line(&mut trigger)?;
    let trigger = trigger.trim().to_string();

    println!("Steps (enter description, empty line to finish):");
    let mut steps = Vec::new();
    let mut order = 1u32;
    loop {
        print!("  Step {}: ", order);
        io::stdout().flush()?;
        let mut step_desc = String::new();
        io::stdin().read_line(&mut step_desc)?;
        let step_desc = step_desc.trim().to_string();
        if step_desc.is_empty() {
            break;
        }

        print!("    Command (optional): ");
        io::stdout().flush()?;
        let mut cmd = String::new();
        io::stdin().read_line(&mut cmd)?;
        let cmd = cmd.trim().to_string();

        steps.push(Step {
            order,
            description: step_desc,
            command: if cmd.is_empty() { None } else { Some(cmd) },
            tool: None,
            needs_approval: false,
            on_failure: FailureAction::Abort,
        });
        order += 1;
    }

    let workflow = mur_common::workflow::Workflow {
        base: KnowledgeBase {
            name: name.clone(),
            description: desc,
            content: Content::Plain(trigger.clone()),
            ..Default::default()
        },
        steps,
        variables: vec![],
        source_sessions: vec![],
        trigger,
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
    };

    store.save(&workflow)?;
    println!("Created workflow: {}", name);
    Ok(())
}

fn cmd_evolve(dry_run: bool, _force: bool) -> Result<()> {
    use evolve::decay::{apply_decay_all, apply_decay_all_dry_run};
    use evolve::maturity::{apply_maturity_all, apply_maturity_all_dry_run};
    use mur_common::knowledge::Maturity;

    let store = YamlStore::default_store()?;
    let now = chrono::Utc::now();

    if dry_run {
        println!("🔮 Evolve (dry run) — previewing changes...\n");
    } else {
        println!("🔮 Evolving patterns...\n");
    }

    // Phase 1: Decay
    let decay_report = if dry_run {
        apply_decay_all_dry_run(&store, now)?
    } else {
        apply_decay_all(&store, now)?
    };

    if !decay_report.details.is_empty() {
        println!("📉 Decay:");
        for d in &decay_report.details {
            let archived = if d.auto_archived { " → ARCHIVED" } else { "" };
            println!(
                "  {} — confidence {:.0}% → {:.0}%{}",
                d.name,
                d.old_confidence * 100.0,
                d.new_confidence * 100.0,
                archived,
            );
        }
        println!();
    }

    // Phase 2: Maturity
    let maturity_report = if dry_run {
        apply_maturity_all_dry_run(&store, now)?
    } else {
        apply_maturity_all(&store, now)?
    };

    if !maturity_report.details.is_empty() {
        println!("🎯 Maturity:");
        for d in &maturity_report.details {
            let arrow = if d.is_promotion { "⬆" } else { "⬇" };
            println!(
                "  {} {} — {:?} → {:?}",
                arrow, d.name, d.old_maturity, d.new_maturity,
            );
        }
        println!();
    }

    // Summary
    let mode = if dry_run { " (dry run)" } else { "" };
    println!("── Summary{} ──", mode);
    println!("  Scanned:        {}", decay_report.patterns_scanned);
    println!("  Decayed:        {}", decay_report.patterns_decayed);
    println!("  Auto-archived:  {}", decay_report.patterns_archived);
    println!("  Promotions:     {}", maturity_report.promotions);
    println!("  Demotions:      {}", maturity_report.demotions);

    // Show maturity distribution
    let patterns = store.list_all()?;
    let (mut draft, mut emerging, mut stable, mut canonical) = (0usize, 0, 0, 0);
    for p in &patterns {
        match p.maturity {
            Maturity::Draft => draft += 1,
            Maturity::Emerging => emerging += 1,
            Maturity::Stable => stable += 1,
            Maturity::Canonical => canonical += 1,
        }
    }
    println!(
        "  Maturity:       Draft: {} | Emerging: {} | Stable: {} | Canonical: {}",
        draft, emerging, stable, canonical
    );

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
