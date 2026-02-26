use anyhow::Result;
use clap::{Parser, Subcommand};
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::io::{self, Write};
use tracing_subscriber::EnvFilter;

mod auth;
mod capture;
mod community;
mod dashboard;
mod evolve;
mod inject;
mod interactive;
mod migrate;
mod retrieve;
mod server;
mod session;
mod store;

use store::workflow_yaml::WorkflowYamlStore;
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
    New {
        /// Path to a diagram file to attach (mermaid, plantuml)
        #[arg(long)]
        diagram: Option<String>,
    },
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
    Sync {
        /// Suppress output
        #[arg(long)]
        quiet: bool,
    },
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
    /// View and manage individual patterns
    Pattern {
        #[command(subcommand)]
        action: PatternAction,
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
    /// Detect emergent patterns from cross-session behaviors
    Emerge {
        /// Minimum number of sessions for a behavior to be considered emergent
        #[arg(long, default_value = "3")]
        threshold: usize,
        /// Preview candidates without creating patterns
        #[arg(long)]
        dry_run: bool,
    },
    /// Show workflow composition & decomposition suggestions
    Suggest {
        /// Auto-create suggested workflows/patterns as drafts
        #[arg(long)]
        create: bool,
    },
    /// Terminal dashboard
    Dashboard,
    /// Inject context-aware patterns (auto-detects project from pwd)
    Context {
        /// Compact output (shorter, fewer patterns)
        #[arg(long)]
        compact: bool,
        /// Override auto-detected query with explicit one
        #[arg(long)]
        query: Option<String>,
        /// Write context to ~/.mur/context.md for file-based tools (Aider, Cline, Windsurf)
        #[arg(long)]
        file: bool,
    },
    /// Session recording for Claude Code hooks
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Community publish/fetch
    Community {
        #[command(subcommand)]
        action: CommunityAction,
    },
    /// Log in to mur community
    Login,
    /// Log out from mur community
    Logout,
    /// Initialize MUR directory and optionally install hooks
    Init {
        /// Install Claude Code hooks (PostToolUse, Stop, UserPromptSubmit)
        #[arg(long)]
        hooks: bool,
    },
    /// Start local API server for web dashboard
    Serve {
        /// Port to listen on
        #[arg(long, default_value = "3847")]
        port: u16,
        /// Open browser after starting
        #[arg(long)]
        open: bool,
        /// Read-only mode (reject all write operations)
        #[arg(long)]
        readonly: bool,
    },
    /// Explain why a pattern was (or would be) injected
    Why {
        /// Pattern name
        name: String,
    },
    /// View and edit a pattern with preview and diff
    Edit {
        /// Pattern name
        name: String,
        /// Quick inline field edit (skip $EDITOR)
        #[arg(long)]
        quick: bool,
    },
}

#[derive(Subcommand)]
enum LearnAction {
    /// Extract patterns from a session transcript
    Extract {
        #[arg(short, long)]
        file: Option<String>,
        /// Also extract and save behavior fingerprints for emergence detection
        #[arg(long)]
        fingerprint: bool,
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
enum PatternAction {
    /// Show a pattern by name (with attachments)
    Show { name: String },
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
enum SessionAction {
    /// Start recording a session
    Start {
        /// Source identifier (e.g. claude-code)
        #[arg(long, default_value = "claude-code")]
        source: String,
    },
    /// Stop recording the active session
    Stop {
        /// Run fingerprint extraction on the recording
        #[arg(long)]
        analyze: bool,
    },
    /// Record an event to the active session
    Record {
        /// Event type: user, assistant, tool_call, tool_result
        #[arg(long, name = "type")]
        event_type: String,
        /// Tool name (for tool_call/tool_result events)
        #[arg(long)]
        tool: Option<String>,
        /// Event content
        #[arg(long)]
        content: String,
    },
    /// Show active session status
    Status,
    /// List past session recordings
    List,
}

#[derive(Subcommand)]
enum CommunityAction {
    /// Publish a pattern to the community
    Publish { name: String },
    /// Fetch (copy) a community pattern by ID
    Fetch { id: String },
    /// Search community patterns
    Search { query: String },
    /// List community patterns
    List {
        /// Sort order: popular, recent, trending, stars
        #[arg(long, default_value = "popular")]
        sort: String,
    },
    /// Star a community pattern
    Star { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("warn,lance=warn,lancedb=warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();

    match cli.command {
        Commands::New { diagram } => cmd_new(diagram)?,
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
            LearnAction::Extract { file, fingerprint } => {
                cmd_learn_extract(file, fingerprint)?;
            }
        },
        Commands::Sync { quiet } => cmd_sync(quiet)?,
        Commands::Inject { query, project: _ } => cmd_inject(&query).await?,
        Commands::Pattern { action } => match action {
            PatternAction::Show { name } => cmd_pattern_show(&name)?,
        },
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
        Commands::Emerge { threshold, dry_run } => cmd_emerge(threshold, dry_run)?,
        Commands::Suggest { create } => cmd_suggest(create)?,
        Commands::Context { compact, query, file } => cmd_context(query, compact, file).await?,
        Commands::Session { action } => match action {
            SessionAction::Start { source } => cmd_session_start(&source)?,
            SessionAction::Stop { analyze } => cmd_session_stop(analyze)?,
            SessionAction::Record {
                event_type,
                tool,
                content,
            } => cmd_session_record(&event_type, tool.as_deref(), &content)?,
            SessionAction::Status => cmd_session_status()?,
            SessionAction::List => cmd_session_list()?,
        },
        Commands::Dashboard => {
            dashboard::render_dashboard()?;
        }
        Commands::Community { action } => match action {
            CommunityAction::Publish { name } => cmd_community_publish(&name).await?,
            CommunityAction::Fetch { id } => cmd_community_fetch(&id).await?,
            CommunityAction::Search { query } => cmd_community_search(&query).await?,
            CommunityAction::List { sort } => cmd_community_list(&sort).await?,
            CommunityAction::Star { id } => cmd_community_star(&id).await?,
        },
        Commands::Login => cmd_login().await?,
        Commands::Logout => cmd_logout()?,
        Commands::Init { hooks } => cmd_init(hooks)?,
        Commands::Serve {
            port,
            open,
            readonly,
        } => cmd_serve(port, open, readonly).await?,
        Commands::Why { name } => cmd_why(&name)?,
        Commands::Edit { name, quick } => cmd_edit(&name, quick)?,
    }

    Ok(())
}

// ─── Command implementations ───────────────────────────────────────

fn cmd_new(diagram_path: Option<String>) -> Result<()> {
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
    let mut technical = read_multiline()?;
    if technical.len() > Content::MAX_LAYER_CHARS {
        println!(
            "⚠️  Technical content truncated to {} chars.",
            Content::MAX_LAYER_CHARS
        );
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
        attachments,
    };

    store.save(&pattern)?;
    println!("✅ Created pattern: {}", name);
    Ok(())
}

fn cmd_search(query: &str) -> Result<()> {
    use retrieve::gate::{GateDecision, evaluate_query};
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
    use inject::hook::{HookTrigger, detect_trigger};
    use retrieve::gate::{GateDecision, evaluate_query};
    use retrieve::scoring::{score_and_rank, score_and_rank_hybrid};
    use std::collections::HashMap;
    use store::embedding::{EmbeddingConfig, embed};
    use store::lancedb::VectorStore;

    // Detect trigger type
    let trigger = detect_trigger(query);
    match &trigger {
        HookTrigger::OnError => {
            eprintln!("# Trigger: OnError — searching for error-related patterns")
        }
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
                let vector_store =
                    VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
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

    let output = inject::hook::format_unified_injection_with_store(
        &injected_patterns,
        &workflows,
        2000,
        Some(&yaml_store),
    );

    if output.is_empty() {
        eprintln!("# No relevant patterns found");
    } else {
        // Record what was injected for post-session feedback analysis
        let project_name = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        inject::hook::record_injection(query, &project_name, &injected_patterns);

        // Record co-occurrence for pattern↔workflow intelligence
        inject::hook::record_cooccurrence_for_injection(&injected_patterns);

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
    use evolve::feedback::{FeedbackSignal, apply_feedback};

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
    use capture::feedback::{SignalType, analyze_session_feedback, read_injection_record};
    use evolve::feedback::{FeedbackSignal, apply_feedback};

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

fn cmd_gc(auto: bool) -> Result<()> {
    use evolve::decay::apply_decay_all;
    use evolve::lifecycle::{LifecycleAction, apply_lifecycle_action, evaluate_lifecycle};
    use evolve::maturity::apply_maturity_all;
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

    // Link discovery pass (pattern↔pattern and pattern↔workflow)
    {
        use evolve::linker::{
            LinkType, apply_workflow_links, discover_links, discover_workflow_links,
        };
        let all = store.list_all()?;
        let wf_store = WorkflowYamlStore::default_store()?;
        let workflows = wf_store.list_all()?;

        // Phase 1: Collect all pattern↔pattern link suggestions (read-only)
        let mut pairs: Vec<(String, String, LinkType)> = Vec::new();
        for pattern in &all {
            for s in discover_links(pattern, &all) {
                pairs.push((pattern.name.clone(), s.target_name, s.link_type));
            }
        }

        // Phase 1b: Collect pattern↔workflow link suggestions
        let mut wf_links: Vec<(String, Vec<String>)> = Vec::new();
        if !workflows.is_empty() {
            for pattern in &all {
                let suggestions = discover_workflow_links(pattern, &workflows);
                if !suggestions.is_empty() {
                    let wf_names: Vec<String> = suggestions
                        .iter()
                        .map(|s| s.workflow_name.clone())
                        .collect();
                    wf_links.push((pattern.name.clone(), wf_names));
                }
            }
        }

        let has_pattern_links = !pairs.is_empty();
        let has_workflow_links = !wf_links.is_empty();

        if has_pattern_links || has_workflow_links {
            // Phase 2: Apply pattern↔pattern links
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

            // Phase 2b: Apply pattern↔workflow links
            for (pattern_name, wf_names) in &wf_links {
                if let Some(p) = by_name.get_mut(pattern_name) {
                    let suggestions: Vec<evolve::linker::WorkflowLinkSuggestion> = wf_names
                        .iter()
                        .map(|name| evolve::linker::WorkflowLinkSuggestion {
                            workflow_name: name.clone(),
                            score: 0.0, // score not needed for apply
                        })
                        .collect();
                    let before = p.links.workflows.len();
                    apply_workflow_links(p, &suggestions);
                    let added = p.links.workflows.len() - before;
                    if added > 0 {
                        changed.insert(pattern_name.clone());
                        link_count += added;
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

    println!("🧹 Found {} patterns for cleanup:\n", candidates.len());
    for p in &candidates {
        let reason = if p.confidence < 0.5 {
            format!("low confidence ({:.0}%)", p.confidence * 100.0)
        } else {
            format!(
                "low effectiveness ({:.0}%)",
                p.evidence.effectiveness() * 100.0
            )
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

    println!("⬆️  Promoted '{}': {:?} → {:?}", name, old_tier, new_tier);
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

fn cmd_sync(quiet: bool) -> Result<()> {
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
        if !quiet {
            println!("No patterns to sync.");
        }
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
        if !quiet {
            println!(
                "  {} — wrote {} patterns to {}",
                target.name,
                top.len(),
                target_path.display()
            );
        }
    }

    if !quiet {
        println!("Sync complete.");
    }
    Ok(())
}

async fn cmd_reindex() -> Result<()> {
    use store::embedding::{EmbeddingConfig, embed};
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
        let mut text = format!(
            "{}: {}\n{}",
            pattern.name,
            pattern.description,
            pattern.content.as_text()
        );
        // Include attachment descriptions in embedding text for better search
        for att in &pattern.attachments {
            if !att.description.is_empty() {
                text.push_str("\n\n");
                text.push_str(&att.description);
            }
        }
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
        let text = format!(
            "{}: {}\n{}",
            workflow.name,
            workflow.description,
            workflow.content.as_text()
        );
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
    vector_store
        .build_unified_index(&indexed_patterns, &indexed_workflows)
        .await?;

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

fn cmd_pattern_show(name: &str) -> Result<()> {
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

fn cmd_workflow_new() -> Result<()> {
    use mur_common::workflow::{FailureAction, Step};

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

fn cmd_suggest(create: bool) -> Result<()> {
    use evolve::compose::suggest_workflows_with_patterns;
    use evolve::cooccurrence::CooccurrenceMatrix;
    use evolve::decompose::{analyze_workflow_for_extraction, extract_pattern_from_step};

    let pattern_store = YamlStore::default_store()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let patterns = pattern_store.list_all()?;
    let workflows = workflow_store.list_all()?;

    // ─── Part 1: Workflow composition from co-occurrence ─────────────

    let matrix_path = CooccurrenceMatrix::default_path();
    let matrix = CooccurrenceMatrix::load(&matrix_path)?;

    println!("🔗 Knowledge ↔ Workflow Intelligence\n");
    println!("── Co-occurrence Data ──");
    println!("  Tracked pairs: {}", matrix.pair_count());

    let suggestions = suggest_workflows_with_patterns(&matrix, 5, &patterns);

    if suggestions.is_empty() {
        println!("  No workflow composition suggestions yet.");
        println!("  (Need 3+ patterns co-occurring 5+ times)");
    } else {
        println!("\n── Workflow Composition Suggestions ──\n");
        for (i, s) in suggestions.iter().enumerate() {
            println!(
                "  {}. {} (score: {})",
                i + 1,
                s.suggested_name,
                s.cooccurrence_score,
            );
            println!("     Patterns: {}", s.patterns.join(", "));
            println!("     Trigger: {}", s.suggested_trigger);

            if create {
                if workflow_store.exists(&s.suggested_name) {
                    println!(
                        "     -> Workflow '{}' already exists, skipping.",
                        s.suggested_name
                    );
                } else {
                    // Create a draft workflow from the suggestion
                    let wf = mur_common::workflow::Workflow {
                        base: KnowledgeBase {
                            name: s.suggested_name.clone(),
                            description: format!(
                                "Auto-suggested workflow from {} co-occurring patterns",
                                s.patterns.len()
                            ),
                            content: Content::Plain(format!(
                                "Combines patterns: {}",
                                s.patterns.join(", ")
                            )),
                            tags: collect_tags_from_patterns(&s.patterns, &patterns),
                            ..Default::default()
                        },
                        steps: vec![],
                        variables: vec![],
                        source_sessions: vec![],
                        trigger: s.suggested_trigger.clone(),
                        tools: vec![],
                        published_version: 0,
                        permission: Default::default(),
                    };
                    workflow_store.save(&wf)?;
                    println!("     -> Created draft workflow: {}", s.suggested_name);

                    // Add cross-reference: link each source pattern to this workflow
                    for pname in &s.patterns {
                        if let Ok(mut p) = pattern_store.get(pname)
                            && !p.links.workflows.contains(&s.suggested_name)
                        {
                            p.base.links.workflows.push(s.suggested_name.clone());
                            let _ = pattern_store.save(&p);
                        }
                    }
                }
            }
            println!();
        }
    }

    // ─── Part 2: Workflow decomposition into patterns ────────────────

    if !workflows.is_empty() {
        println!("── Decomposition Candidates ──\n");

        let mut any_candidates = false;
        for wf in &workflows {
            let candidates = analyze_workflow_for_extraction(wf, &patterns);
            if candidates.is_empty() {
                continue;
            }
            any_candidates = true;

            println!("  Workflow: {} ({} candidates)", wf.name, candidates.len());
            for c in &candidates {
                println!("    Step {}: \"{}\"", c.step_index + 1, c.step_description,);
                println!("      -> Pattern: {}", c.suggested_pattern_name);
                println!("      Reason: {}", c.reason);

                if create {
                    if pattern_store.exists(&c.suggested_pattern_name) {
                        println!(
                            "      -> Pattern '{}' already exists, skipping.",
                            c.suggested_pattern_name
                        );
                    } else if let Some(pattern) = extract_pattern_from_step(wf, c.step_index) {
                        pattern_store.save(&pattern)?;
                        println!(
                            "      -> Created draft pattern: {}",
                            c.suggested_pattern_name
                        );
                    }
                }
            }
            println!();
        }

        if !any_candidates {
            println!("  No decomposition candidates found in existing workflows.");
        }
    }

    // ─── Summary ─────────────────────────────────────────────────────

    if !create && (!suggestions.is_empty() || !workflows.is_empty()) {
        println!("Run `mur suggest --create` to auto-create suggested items as drafts.");
    }

    Ok(())
}

/// Collect tags from a set of pattern names.
fn collect_tags_from_patterns(names: &[String], patterns: &[Pattern]) -> mur_common::pattern::Tags {
    let mut topics: Vec<String> = Vec::new();
    let mut languages: Vec<String> = Vec::new();

    for name in names {
        if let Some(p) = patterns.iter().find(|p| &p.name == name) {
            for t in &p.tags.topics {
                if !topics.contains(t) {
                    topics.push(t.clone());
                }
            }
            for l in &p.tags.languages {
                if !languages.contains(l) {
                    languages.push(l.clone());
                }
            }
        }
    }

    mur_common::pattern::Tags {
        topics,
        languages,
        extra: Default::default(),
    }
}

fn cmd_learn_extract(file: Option<String>, fingerprint: bool) -> Result<()> {
    // Read transcript
    let transcript = match file {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read transcript file '{}': {}", path, e))?,
        None => {
            let mut buf = String::new();
            io::Read::read_to_string(&mut io::stdin(), &mut buf)?;
            buf
        }
    };

    if transcript.trim().is_empty() {
        println!("Empty transcript. Provide a session transcript via --file or stdin.");
        return Ok(());
    }

    // Pattern extraction still requires LLM integration
    println!("Pattern extraction requires LLM integration (coming soon).");

    // Fingerprint extraction (no LLM needed)
    if fingerprint {
        use capture::emergence::{extract_fingerprints, prune_fingerprints, save_fingerprints};

        let session_id = uuid::Uuid::new_v4().to_string();
        let fps = extract_fingerprints(&transcript, &session_id);

        if fps.is_empty() {
            println!("No behavior fingerprints detected in transcript.");
        } else {
            save_fingerprints(&fps)?;
            println!(
                "Extracted {} behavior fingerprints from session {}",
                fps.len(),
                &session_id[..8]
            );

            // Auto-prune fingerprints older than 90 days
            let pruned = prune_fingerprints(90)?;
            if pruned > 0 {
                println!("Pruned {} fingerprints older than 90 days.", pruned);
            }
        }
    }

    Ok(())
}

fn cmd_emerge(threshold: usize, dry_run: bool) -> Result<()> {
    use capture::emergence::{detect_emergent, load_fingerprints, prune_fingerprints};

    // Auto-prune old fingerprints first
    let pruned = prune_fingerprints(90)?;
    if pruned > 0 {
        println!("Pruned {} fingerprints older than 90 days.\n", pruned);
    }

    // Load all fingerprints
    let fingerprints = load_fingerprints()?;
    if fingerprints.is_empty() {
        println!(
            "No fingerprints found. Run `mur learn extract --fingerprint` on session transcripts first."
        );
        return Ok(());
    }

    let session_count: std::collections::HashSet<&str> = fingerprints
        .iter()
        .map(|fp| fp.session_id.as_str())
        .collect();

    println!(
        "Loaded {} fingerprints from {} sessions.\n",
        fingerprints.len(),
        session_count.len()
    );

    // Detect emergent patterns
    let candidates = detect_emergent(&fingerprints, threshold);

    if candidates.is_empty() {
        println!(
            "No emergent patterns found (threshold: {} sessions).\nKeep running `mur learn extract --fingerprint` to build up the fingerprint database.",
            threshold
        );
        return Ok(());
    }

    let mode = if dry_run { " (dry run)" } else { "" };
    println!(
        "Found {} emergent patterns from {} sessions{}\n",
        candidates.len(),
        session_count.len(),
        mode
    );

    let store = YamlStore::default_store()?;
    let mut created = 0;

    for (i, candidate) in candidates.iter().enumerate() {
        println!(
            "{}. {} (seen in {} sessions)",
            i + 1,
            candidate.suggested_name,
            candidate.session_count
        );
        println!("   Behavior: {}", candidate.behavior);
        println!("   Keywords: {}", candidate.keywords.join(", "));
        println!("   Sessions: {}", candidate.session_ids.join(", "));

        if !candidate.evidence.is_empty() {
            println!("   Evidence:");
            for ev in &candidate.evidence {
                println!("     - {}", ev);
            }
        }

        if !dry_run {
            // Create a draft pattern
            let name = &candidate.suggested_name;
            if store.exists(name) {
                println!("   -> Pattern '{}' already exists, skipping.", name);
            } else {
                let pattern = Pattern {
                    base: KnowledgeBase {
                        schema: mur_common::pattern::SCHEMA_VERSION,
                        name: name.clone(),
                        description: format!(
                            "Emergent: {} (detected across {} sessions)",
                            candidate.behavior, candidate.session_count
                        ),
                        content: mur_common::pattern::Content::DualLayer {
                            technical: candidate.suggested_content.clone(),
                            principle: None,
                        },
                        tier: mur_common::pattern::Tier::Session,
                        importance: 0.3,
                        confidence: 0.2,
                        tags: mur_common::pattern::Tags {
                            languages: vec![],
                            topics: candidate.keywords.clone(),
                            extra: Default::default(),
                        },
                        evidence: mur_common::pattern::Evidence {
                            source_sessions: candidate.session_ids.clone(),
                            first_seen: Some(chrono::Utc::now()),
                            ..Default::default()
                        },
                        maturity: mur_common::knowledge::Maturity::Draft,
                        ..Default::default()
                    },
                    attachments: vec![],
                };
                store.save(&pattern)?;
                println!("   -> Created draft pattern: {}", name);
                created += 1;
            }
        }

        println!();
    }

    if dry_run {
        println!("Run without --dry-run to create these as draft patterns.");
    } else if created > 0 {
        println!(
            "Created {} draft patterns (maturity: Draft, confidence: 0.2).",
            created
        );
    }

    Ok(())
}

// ─── Context command ──────────────────────────────────────────────

async fn cmd_context(query: Option<String>, compact: bool, write_file: bool) -> Result<()> {
    use retrieve::scoring::{score_and_rank, score_and_rank_hybrid};
    use std::collections::HashMap;
    use store::embedding::{EmbeddingConfig, embed};
    use store::lancedb::VectorStore;

    // Auto-detect project context from cwd
    let cwd = std::env::current_dir()?;
    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // Build query from provided or auto-detected context
    let effective_query = match query {
        Some(q) => q,
        None => {
            // Auto-detect: project name + recent git context
            let mut parts = vec![project_name.clone()];

            // Try to get git remote for additional context
            if let Ok(output) = std::process::Command::new("git")
                .args(["remote", "get-url", "origin"])
                .current_dir(&cwd)
                .output()
                && output.status.success()
            {
                let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
                // Extract repo name from URL
                if let Some(name) = remote.rsplit('/').next() {
                    let name = name.trim_end_matches(".git");
                    if name != project_name {
                        parts.push(name.to_string());
                    }
                }
            }

            // Try to get recent file context
            if let Ok(output) = std::process::Command::new("git")
                .args(["diff", "--name-only", "HEAD~3..HEAD"])
                .current_dir(&cwd)
                .output()
                && output.status.success()
            {
                let files = String::from_utf8_lossy(&output.stdout);
                for file in files.lines().take(5) {
                    // Extract keywords from file paths
                    let stem = std::path::Path::new(file)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    if !stem.is_empty() && stem.len() > 2 {
                        parts.push(stem.to_string());
                    }
                }
            }

            parts.join(" ")
        }
    };

    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    // Try hybrid search if LanceDB index exists
    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

    let results = if index_path.exists() {
        let cfg = store::config::load_config()?;
        let config = EmbeddingConfig::from_config(&cfg);
        match embed(&effective_query, &config).await {
            Ok(query_embedding) => {
                let vector_store =
                    VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
                let vector_results = vector_store.search(&query_embedding, 20, None).await?;
                let vector_scores: HashMap<String, f64> = vector_results
                    .into_iter()
                    .map(|r| (r.name, r.similarity as f64))
                    .collect();
                score_and_rank_hybrid(&effective_query, patterns, &vector_scores)
            }
            Err(_) => score_and_rank(&effective_query, patterns),
        }
    } else {
        score_and_rank(&effective_query, patterns)
    };

    let mut injected_patterns: Vec<Pattern> = Vec::new();
    for sp in results {
        let mut p = sp.pattern;
        if p.lifecycle.status == LifecycleStatus::Archived {
            continue;
        }
        let now = chrono::Utc::now();
        p.decay.last_active = Some(now);
        p.evidence.injection_count += 1;
        p.lifecycle.last_injected = Some(now);
        p.updated_at = now;
        let _ = yaml_store.save(&p);
        injected_patterns.push(p);
    }

    let token_budget = if compact { 800 } else { 2000 };
    let output = inject::hook::format_unified_injection_with_store(
        &injected_patterns,
        &workflows,
        token_budget,
        Some(&yaml_store),
    );

    if !output.is_empty() {
        inject::hook::record_injection(&effective_query, &project_name, &injected_patterns);
        inject::hook::record_cooccurrence_for_injection(&injected_patterns);

        if write_file {
            // Write to ~/.mur/context.md for file-based tools
            let context_path = dirs::home_dir()
                .expect("no home dir")
                .join(".mur")
                .join("context.md");
            let file_content = format!(
                "# MUR Context (auto-generated)\n# Query: {}\n# Updated: {}\n\n{}\n",
                effective_query,
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
                output
            );
            std::fs::write(&context_path, file_content)?;
            eprintln!("📝 Context written to {}", context_path.display());
        } else {
            print!("{}", output);
        }
    }

    Ok(())
}

// ─── Session commands ─────────────────────────────────────────────

fn cmd_session_start(source: &str) -> Result<()> {
    let session = session::start(source)?;
    eprintln!("Session started: {} (source: {})", &session.id[..8], source);
    Ok(())
}

fn cmd_session_stop(analyze: bool) -> Result<()> {
    match session::stop()? {
        Some(id) => {
            eprintln!("Session stopped: {}", &id[..8]);
            if analyze {
                // Run fingerprint extraction on the recording
                let recording_path = dirs::home_dir()
                    .expect("no home dir")
                    .join(".mur")
                    .join("session")
                    .join("recordings")
                    .join(format!("{}.jsonl", id));

                if recording_path.exists() {
                    let content = std::fs::read_to_string(&recording_path)?;
                    if !content.trim().is_empty() {
                        use capture::emergence::{extract_fingerprints, save_fingerprints};
                        let fps = extract_fingerprints(&content, &id);
                        if !fps.is_empty() {
                            save_fingerprints(&fps)?;
                            eprintln!("Extracted {} fingerprints from session.", fps.len());
                        }
                    }
                }
            }
        }
        None => {
            eprintln!("No active session.");
        }
    }
    Ok(())
}

fn cmd_session_record(event_type: &str, tool: Option<&str>, content: &str) -> Result<()> {
    // Validate event type
    match event_type {
        "user" | "assistant" | "tool_call" | "tool_result" => {}
        _ => anyhow::bail!(
            "Invalid event type '{}'. Use: user, assistant, tool_call, tool_result",
            event_type
        ),
    }

    if !session::record(event_type, tool, content)? {
        // No active session — silently succeed (hooks shouldn't fail)
        return Ok(());
    }
    Ok(())
}

fn cmd_session_status() -> Result<()> {
    match session::get_active()? {
        Some(session) => {
            println!("Active session: {}", session.id);
            println!("  Started: {}", session.started_at);
            println!("  Source:  {}", session.source);

            // Count events in the recording
            let recording_path = dirs::home_dir()
                .expect("no home dir")
                .join(".mur")
                .join("session")
                .join("recordings")
                .join(format!("{}.jsonl", session.id));

            if recording_path.exists() {
                let content = std::fs::read_to_string(&recording_path).unwrap_or_default();
                let count = content.lines().filter(|l| !l.trim().is_empty()).count();
                println!("  Events:  {}", count);
            }
        }
        None => {
            println!("No active session.");
        }
    }
    Ok(())
}

fn cmd_session_list() -> Result<()> {
    let recordings = session::list_recordings()?;

    if recordings.is_empty() {
        println!("No session recordings found.");
        return Ok(());
    }

    println!("Session recordings ({}):\n", recordings.len());
    for r in &recordings {
        let time: chrono::DateTime<chrono::Utc> = r.modified.into();
        let short_id = if r.id.len() > 8 { &r.id[..8] } else { &r.id };
        println!(
            "  {} — {} events, {} bytes ({})",
            short_id,
            r.event_count,
            r.file_size,
            time.format("%Y-%m-%d %H:%M"),
        );
    }
    Ok(())
}

// ─── Init command ─────────────────────────────────────────────────

// ─── Auth commands ───────────────────────────────────────────────

async fn cmd_login() -> Result<()> {
    if let Some(_tokens) = auth::load_tokens() {
        println!("Already logged in. Run `mur logout` first to re-authenticate.");
        return Ok(());
    }

    println!("Logging in to mur community...");
    let client = reqwest::Client::new();
    let tokens = auth::device_code_flow(&client).await?;
    auth::save_tokens(&tokens)?;
    println!();
    println!("  Logged in successfully! Token stored in ~/.mur/auth.json");
    Ok(())
}

fn cmd_logout() -> Result<()> {
    auth::clear_tokens()?;
    println!("Logged out. Auth tokens removed.");
    Ok(())
}

// ─── Community commands ─────────────────────────────────────────

async fn cmd_community_publish(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;

    let description = pattern.base.description.clone();

    let content = match &pattern.base.content {
        Content::DualLayer {
            technical,
            principle,
        } => {
            if let Some(p) = principle {
                format!("{}\n\n---\n\n{}", technical, p)
            } else {
                technical.clone()
            }
        }
        Content::Plain(s) => s.clone(),
    };

    let mut tags: Vec<String> = pattern.base.tags.languages.clone();
    tags.extend(pattern.base.tags.topics.clone());

    let category = pattern.base.tags.topics.first().cloned();

    let client = reqwest::Client::new();
    let resp = community::share(
        &client,
        name,
        &description,
        &content,
        &tags,
        category.as_deref(),
    )
    .await?;

    println!("  Published '{}' to community!", name);
    println!("  Pattern ID: {}", resp.pattern_id);
    Ok(())
}

async fn cmd_community_fetch(id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::copy_pattern(&client, id).await?;

    // Save as a session-tier pattern locally
    let mur_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".mur");
    let patterns_dir = mur_dir.join("patterns");
    std::fs::create_dir_all(&patterns_dir)?;

    let slug = resp.name.to_lowercase().replace(' ', "-");
    let path = patterns_dir.join(format!("{}.yaml", slug));

    // Build a minimal Pattern and save as YAML
    let tags_vec: Vec<String> = match &resp.tags {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => vec![],
    };

    let pattern = mur_common::pattern::Pattern {
        base: KnowledgeBase {
            name: resp.name.clone(),
            description: resp.description.clone(),
            content: Content::Plain(resp.content.clone()),
            tier: Tier::Session,
            tags: Tags {
                languages: vec![],
                topics: tags_vec,
                extra: std::collections::HashMap::new(),
            },
            ..Default::default()
        },
        attachments: vec![],
    };

    let yaml = serde_yaml::to_string(&pattern)?;
    std::fs::write(&path, yaml)?;

    println!("  Fetched '{}' to {}", resp.name, path.display());
    Ok(())
}

async fn cmd_community_search(query: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::search(&client, query).await?;

    if resp.patterns.is_empty() {
        println!("  No patterns found for '{}'", query);
        return Ok(());
    }

    println!("  Found {} pattern(s) for '{}':\n", resp.count, query);
    print_pattern_table(&resp.patterns);
    Ok(())
}

async fn cmd_community_list(sort: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::list(&client, Some(sort)).await?;

    if resp.patterns.is_empty() {
        println!("  No community patterns yet.");
        return Ok(());
    }

    println!(
        "  {} community pattern(s) (sorted by {}):\n",
        resp.count, sort
    );
    print_pattern_table(&resp.patterns);
    Ok(())
}

async fn cmd_community_star(id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    community::star(&client, id).await?;
    println!("  Starred pattern {}", id);
    Ok(())
}

fn print_pattern_table(patterns: &[community::CommunityPattern]) {
    // Header
    println!(
        "  {:<36}  {:<30}  {:>5}  {:>5}  {}",
        "ID", "NAME", "STARS", "COPIES", "AUTHOR"
    );
    println!("  {}", "-".repeat(100));
    for p in patterns {
        let author = p.author_login.as_deref().unwrap_or(&p.author_name);
        println!(
            "  {:<36}  {:<30}  {:>5}  {:>5}  {}",
            p.id,
            truncate(&p.name, 30),
            p.star_count,
            p.copy_count,
            author
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

fn cmd_init(hooks_flag: bool) -> Result<()> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let mur_dir = home.join(".mur");

    // ─── Step A: Create directory structure ───────────────────────
    let dirs_to_create = [
        mur_dir.clone(),
        mur_dir.join("patterns"),
        mur_dir.join("workflows"),
        mur_dir.join("session").join("recordings"),
        mur_dir.join("hooks"),
        mur_dir.join("index"),
    ];
    for d in &dirs_to_create {
        std::fs::create_dir_all(d)?;
    }

    // ─── Step E: Write default config.yaml if not exists ─────────
    let config_path = mur_dir.join("config.yaml");
    if !config_path.exists() {
        let default_config = r#"# MUR Configuration
# See: https://github.com/mur-run/mur

tools:
  claude:
    enabled: true
  gemini:
    enabled: true

search:
  provider: ollama
  model: qwen3-embedding:0.6b

learning:
  llm:
    provider: ollama
    model: llama3.2:3b
"#;
        std::fs::write(&config_path, default_config)?;
    }

    // ─── Determine whether to install hooks ──────────────────────
    let install_hooks = if hooks_flag {
        true
    } else {
        // Interactive prompt
        print!("Install Claude Code hooks? [Y/n] ");
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        let answer = answer.trim().to_lowercase();
        answer.is_empty() || answer == "y" || answer == "yes"
    };

    let mut hooks_installed = Vec::new();

    if install_hooks {
        // ─── Step B: Write hook scripts ──────────────────────────
        let on_prompt = r#"#!/bin/bash
# mur-managed-hook v5
INPUT=$(cat /dev/stdin 2>/dev/null || echo '{}')
MUR=$(which mur 2>/dev/null || echo "mur")
$MUR context --compact 2>/dev/null || true
if [ -f ~/.mur/session/active.json ]; then
  PROMPT=$(echo "$INPUT" | jq -r '.prompt // empty' 2>/dev/null)
  if [ -n "$PROMPT" ]; then
    $MUR session record --event-type user --content "$PROMPT" 2>/dev/null || true
  fi
fi
exit 0
"#;

        let on_tool = r#"#!/bin/bash
# mur-managed-hook v5
MUR=$(which mur 2>/dev/null || echo "mur")
if [ -f ~/.mur/session/active.json ]; then
  INPUT=$(cat /dev/stdin 2>/dev/null || echo '{}')
  TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
  TOOL_INPUT=$(echo "$INPUT" | jq -c '.tool_input // {}' 2>/dev/null)
  if [ -n "$TOOL" ]; then
    $MUR session record --event-type tool_call --tool "$TOOL" --content "$TOOL_INPUT" 2>/dev/null || true
  fi
fi
"#;

        let on_stop = r#"#!/bin/bash
# mur-managed-hook v5
INPUT=$(cat /dev/stdin 2>/dev/null || echo '{}')
MUR=$(which mur 2>/dev/null || echo "mur")
if [ -f ~/.mur/session/active.json ]; then
  STOP_REASON=$(echo "$INPUT" | jq -r '.stop_reason // "turn_end"' 2>/dev/null)
  $MUR session record --event-type assistant --content "[stop: $STOP_REASON]" 2>/dev/null || true
fi
($MUR sync --quiet 2>/dev/null &)
($MUR evolve 2>/dev/null &)
exit 0
"#;

        let hooks = [
            ("on-prompt.sh", on_prompt),
            ("on-tool.sh", on_tool),
            ("on-stop.sh", on_stop),
        ];

        for (filename, content) in &hooks {
            let path = mur_dir.join("hooks").join(filename);
            std::fs::write(&path, content)?;
            // Make executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
            }
        }

        // ─── Step C: Install Claude Code hooks in settings.json ──
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir)?;
        let settings_path = claude_dir.join("settings.json");

        let mut settings: serde_json::Value = if settings_path.exists() {
            let content = std::fs::read_to_string(&settings_path)?;
            serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        let hooks_dir = mur_dir.join("hooks");
        let mur_hook_marker = "mur-managed-hook";

        // Define the hooks we want to install
        let hook_defs = [
            (
                "UserPromptSubmit",
                hooks_dir.join("on-prompt.sh").to_string_lossy().to_string(),
            ),
            (
                "PostToolUse",
                hooks_dir.join("on-tool.sh").to_string_lossy().to_string(),
            ),
            (
                "Stop",
                hooks_dir.join("on-stop.sh").to_string_lossy().to_string(),
            ),
        ];

        let hooks_obj = settings
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (event_name, script_path) in &hook_defs {
            let event_arr = hooks_obj
                .as_object_mut()
                .unwrap()
                .entry(*event_name)
                .or_insert_with(|| serde_json::json!([]));

            let arr = event_arr.as_array_mut().unwrap();

            // Remove any existing mur-managed hooks (by checking command contains mur hooks dir)
            arr.retain(|entry| {
                // Check flat format: { command: "..." }
                if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
                    return !cmd.contains(mur_hook_marker) && !cmd.contains(".mur/hooks/");
                }
                // Check nested format: { hooks: [{ command: "..." }] }
                if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
                    return !hooks.iter().any(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| c.contains(".mur/hooks/"))
                            .unwrap_or(false)
                    });
                }
                true
            });

            // Add our hook (Claude Code format: { hooks: [...], matcher: "" })
            arr.push(serde_json::json!({
                "hooks": [{
                    "type": "command",
                    "command": format!("bash {}", script_path),
                }],
                "matcher": ""
            }));
        }

        // Write settings back with pretty formatting
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;

        hooks_installed.push("Claude Code");
    }

    // ─── Step C2: Install Auggie hooks in settings.json ──────────
    let auggie_dir = home.join(".augment");
    if auggie_dir.exists() {
        let auggie_settings_path = auggie_dir.join("settings.json");
        let mut auggie_settings: serde_json::Value = if auggie_settings_path.exists() {
            let data = std::fs::read_to_string(&auggie_settings_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        // Auggie uses SessionStart / Stop hook events
        let mur_hooks = serde_json::json!({
            "SessionStart": [{
                "hooks": [{"type": "command", "command": format!("bash {}", prompt_script.display())}]
            }],
            "Stop": [{
                "hooks": [{"type": "command", "command": format!("bash {}", stop_script.display())}]
            }]
        });

        // Merge: preserve existing hooks, overwrite mur-managed ones
        let existing_hooks = auggie_settings
            .get("hooks")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let mut merged = existing_hooks.as_object().cloned().unwrap_or_default();
        for (k, v) in mur_hooks.as_object().unwrap() {
            merged.insert(k.clone(), v.clone());
        }
        auggie_settings["hooks"] = serde_json::Value::Object(merged);

        let pretty = serde_json::to_string_pretty(&auggie_settings)?;
        std::fs::write(&auggie_settings_path, pretty)?;
        hooks_installed.push("Auggie");
    }

    // ─── Step C3: Install Gemini CLI hooks in settings.json ──────
    let gemini_dir = home.join(".gemini");
    if gemini_dir.exists() {
        let gemini_settings_path = gemini_dir.join("settings.json");
        let mut gemini_settings: serde_json::Value = if gemini_settings_path.exists() {
            let data = std::fs::read_to_string(&gemini_settings_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        let tool_script = hooks_dir.join("on-tool.sh");

        // Gemini CLI v0.26.0+ hook events
        let mur_hooks = serde_json::json!({
            "BeforeAgent": [{
                "hooks": [{"type": "command", "command": format!("bash {}", prompt_script.display())}]
            }],
            "AfterTool": [{
                "hooks": [{"type": "command", "command": format!("bash {}", tool_script.display())}]
            }],
            "SessionEnd": [{
                "hooks": [{"type": "command", "command": format!("bash {}", stop_script.display())}]
            }]
        });

        let existing_hooks = gemini_settings
            .get("hooks")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let mut merged = existing_hooks.as_object().cloned().unwrap_or_default();
        for (k, v) in mur_hooks.as_object().unwrap() {
            merged.insert(k.clone(), v.clone());
        }
        gemini_settings["hooks"] = serde_json::Value::Object(merged);

        let pretty = serde_json::to_string_pretty(&gemini_settings)?;
        std::fs::write(&gemini_settings_path, pretty)?;
        hooks_installed.push("Gemini CLI");
    }

    // ─── Step C4: Install GitHub Copilot CLI hooks ───────────────
    // Copilot CLI (GA 2026-02-25) reads hooks from:
    //   - ~/.github/hooks.json (global)
    //   - .github/hooks.json (project-level)
    // Format: { version: 1, hooks: { eventName: [{ type, bash, timeoutSec }] } }
    // Events: sessionStart, sessionEnd, userPromptSubmitted, preToolUse, postToolUse
    let copilot_hooks_dir = home.join(".github");
    {
        std::fs::create_dir_all(&copilot_hooks_dir)?;
        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let tool_script = hooks_dir.join("on-tool.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        let hooks_path = copilot_hooks_dir.join("hooks.json");
        let mut hooks_json: serde_json::Value = if hooks_path.exists() {
            let data = std::fs::read_to_string(&hooks_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({"version": 1, "hooks": {}}))
        } else {
            serde_json::json!({"version": 1, "hooks": {}})
        };

        let mur_marker = ".mur/hooks/";
        let hook_defs = [
            ("sessionStart", format!("bash {}", prompt_script.display())),
            ("userPromptSubmitted", format!("bash {}", prompt_script.display())),
            ("postToolUse", format!("bash {}", tool_script.display())),
            ("sessionEnd", format!("bash {}", stop_script.display())),
        ];

        let hooks_obj = hooks_json
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (event_name, script_cmd) in &hook_defs {
            let event_arr = hooks_obj
                .as_object_mut()
                .unwrap()
                .entry(*event_name)
                .or_insert_with(|| serde_json::json!([]));
            let arr = event_arr.as_array_mut().unwrap();
            // Remove existing mur hooks
            arr.retain(|entry| {
                entry.get("bash").and_then(|c| c.as_str())
                    .map(|c| !c.contains(mur_marker))
                    .unwrap_or(true)
            });
            arr.push(serde_json::json!({
                "type": "command",
                "bash": script_cmd,
                "comment": "mur-managed-hook",
                "timeoutSec": 30
            }));
        }

        let pretty = serde_json::to_string_pretty(&hooks_json)?;
        std::fs::write(&hooks_path, pretty)?;
        hooks_installed.push("Copilot CLI");
    }

    // ─── Step C5: Install OpenClaw hooks ─────────────────────────
    let openclaw_config_path = home.join(".openclaw").join("config.json");
    if openclaw_config_path.exists() {
        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        let mut oc_config: serde_json::Value = {
            let data = std::fs::read_to_string(&openclaw_config_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({}))
        };

        // OpenClaw uses a hooks array in config.json
        let mur_hooks = serde_json::json!([
            {
                "id": "mur-on-prompt",
                "event": "session.start",
                "command": format!("bash {}", prompt_script.display())
            },
            {
                "id": "mur-on-stop",
                "event": "session.end",
                "command": format!("bash {}", stop_script.display())
            }
        ]);

        // Replace existing mur hooks, keep others
        let existing_hooks = oc_config
            .get("hooks")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();
        let mut kept: Vec<serde_json::Value> = existing_hooks
            .into_iter()
            .filter(|h| {
                h.get("id")
                    .and_then(|id| id.as_str())
                    .map(|id| !id.starts_with("mur-"))
                    .unwrap_or(true)
            })
            .collect();
        if let Some(arr) = mur_hooks.as_array() {
            kept.extend(arr.clone());
        }
        oc_config["hooks"] = serde_json::Value::Array(kept);

        let pretty = serde_json::to_string_pretty(&oc_config)?;
        std::fs::write(&openclaw_config_path, pretty)?;
        hooks_installed.push("OpenClaw");
    }

    // ─── Step C6: Install Cursor hooks ────────────────────────────
    let cursor_dir = home.join(".cursor");
    if cursor_dir.exists() {
        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let tool_script = hooks_dir.join("on-tool.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        let cursor_hooks_path = cursor_dir.join("hooks.json");
        let mut cursor_hooks: serde_json::Value = if cursor_hooks_path.exists() {
            let data = std::fs::read_to_string(&cursor_hooks_path)?;
            serde_json::from_str(&data).unwrap_or(serde_json::json!({"version": 1, "hooks": {}}))
        } else {
            serde_json::json!({"version": 1, "hooks": {}})
        };

        let mur_hook_marker = "mur-managed-hook";

        // Cursor hooks format: { version: 1, hooks: { eventName: [{ command: "..." }] } }
        let hook_defs = [
            ("beforeSubmitPrompt", prompt_script.to_string_lossy().to_string()),
            ("beforeShellExecution", tool_script.to_string_lossy().to_string()),
            ("stop", stop_script.to_string_lossy().to_string()),
        ];

        let hooks_obj = cursor_hooks
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (event_name, script_path) in &hook_defs {
            let event_arr = hooks_obj
                .as_object_mut()
                .unwrap()
                .entry(*event_name)
                .or_insert_with(|| serde_json::json!([]));
            let arr = event_arr.as_array_mut().unwrap();
            arr.retain(|entry| {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| !c.contains(mur_hook_marker) && !c.contains(".mur/hooks/"))
                    .unwrap_or(true)
            });
            arr.push(serde_json::json!({
                "command": format!("bash {}", script_path)
            }));
        }

        let pretty = serde_json::to_string_pretty(&cursor_hooks)?;
        std::fs::write(&cursor_hooks_path, pretty)?;
        hooks_installed.push("Cursor");
    }

    // ─── Step C7: Install Codex CLI integration ──────────────────
    let codex_dir = home.join(".codex");
    if codex_dir.exists() {
        // Codex reads AGENTS.md — we add a mur context section
        // Also set developer_instructions in config.toml
        let config_path = codex_dir.join("config.toml");
        if config_path.exists() {
            let mut config_content = std::fs::read_to_string(&config_path)?;
            let mur_instruction = "# mur-managed: inject learning context\n# Run `mur context --compact` before sessions for pattern injection\n";
            if !config_content.contains("mur-managed") {
                config_content.push_str(&format!(
                    "\n{}\ndeveloper_instructions = \"Before coding, check if mur has relevant patterns: run `mur context --compact` in the project directory.\"\n",
                    mur_instruction
                ));
                std::fs::write(&config_path, config_content)?;
            }
        }
        hooks_installed.push("Codex CLI");
    }

    // ─── Step C8a: Install OpenCode plugin ─────────────────────────
    // OpenCode uses JS/TS plugins in ~/.config/opencode/plugins/
    let opencode_plugins = home.join(".config").join("opencode").join("plugins");
    if home.join(".config").join("opencode").exists() || std::process::Command::new("which")
        .arg("opencode")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        std::fs::create_dir_all(&opencode_plugins)?;
        let plugin_path = opencode_plugins.join("mur-plugin.ts");
        let hooks_dir = mur_dir.join("hooks");
        let plugin_content = format!(r#"// MUR learning plugin for OpenCode
// Auto-generated by `mur init --hooks`
import {{ execSync }} from "child_process";

export const MurPlugin = async ({{ project, $ }}) => {{
  // Inject MUR context at session start
  try {{
    execSync("bash {on_prompt}", {{ stdio: "pipe", timeout: 30000 }});
  }} catch (_) {{}}

  return {{
    "session.created": async (_input) => {{
      try {{
        execSync("bash {on_prompt}", {{ stdio: "pipe", timeout: 30000 }});
      }} catch (_) {{}}
    }},
    "tool.execute.after": async (_input) => {{
      try {{
        execSync("bash {on_tool}", {{ stdio: "pipe", timeout: 10000 }});
      }} catch (_) {{}}
    }},
    "session.updated": async (input) => {{
      // On session end, trigger learning
      if (input?.status === "complete" || input?.status === "error") {{
        try {{
          execSync("bash {on_stop}", {{ stdio: "pipe", timeout: 30000 }});
        }} catch (_) {{}}
      }}
    }},
  }};
}};
"#,
            on_prompt = hooks_dir.join("on-prompt.sh").display(),
            on_tool = hooks_dir.join("on-tool.sh").display(),
            on_stop = hooks_dir.join("on-stop.sh").display(),
        );
        std::fs::write(&plugin_path, plugin_content)?;
        hooks_installed.push("OpenCode");
    }

    // ─── Step C8b: Install Amp hooks ──────────────────────────────
    // Amp uses Claude Code hook format in AGENTS.md frontmatter or ~/.amp/hooks.json
    // Also supports .agents/skills/ for skills
    let amp_exists = std::process::Command::new("which")
        .arg("amp")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if amp_exists {
        let amp_dir = home.join(".amp");
        std::fs::create_dir_all(&amp_dir)?;
        let hooks_dir = mur_dir.join("hooks");
        let prompt_script = hooks_dir.join("on-prompt.sh");
        let tool_script = hooks_dir.join("on-tool.sh");
        let stop_script = hooks_dir.join("on-stop.sh");

        // Amp uses same format as Claude Code hooks
        let hooks_path = amp_dir.join("hooks.json");
        let amp_hooks = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "hooks": [{"type": "command", "command": format!("bash {}", prompt_script.display())}],
                    "matcher": ""
                }],
                "PostToolUse": [{
                    "hooks": [{"type": "command", "command": format!("bash {}", tool_script.display())}],
                    "matcher": ""
                }],
                "Stop": [{
                    "hooks": [{"type": "command", "command": format!("bash {}", stop_script.display())}],
                    "matcher": ""
                }]
            }
        });
        let pretty = serde_json::to_string_pretty(&amp_hooks)?;
        std::fs::write(&hooks_path, pretty)?;
        hooks_installed.push("Amp");
    }

    // ─── Step C9: Generate context files for file-based tools ────
    // Aider, Cline, Windsurf, Amazon Q use file-based instructions
    // Generate a shared mur context file that can be referenced
    let mur_context_path = mur_dir.join("context.md");
    let mur_context = r#"# MUR Context
# Auto-generated by `mur init --hooks`. Updated by `mur context --file`.
# This file is referenced by Aider, Cline, Windsurf, and other file-based tools.

## How to use MUR with this tool

MUR captures learning patterns from your coding sessions.
Run `mur context` to see relevant patterns for your current project.
Run `mur search <query>` to find specific patterns.
Run `mur learn` to extract new patterns from recent sessions.

## Quick reference

- Patterns: ~/.mur/patterns/
- Workflows: ~/.mur/workflows/
- Dashboard: `mur serve --open`
"#;
    std::fs::write(&mur_context_path, mur_context)?;

    // Aider: add to .aider.conf.yml if it exists
    let aider_conf = home.join(".aider.conf.yml");
    if aider_conf.exists() {
        let content = std::fs::read_to_string(&aider_conf)?;
        if !content.contains(".mur/context.md") {
            let mut new_content = content;
            new_content.push_str(&format!(
                "\n# mur-managed: auto-load learning context\nread:\n  - {}\n",
                mur_context_path.display()
            ));
            std::fs::write(&aider_conf, new_content)?;
            hooks_installed.push("Aider");
        }
    } else {
        // Create minimal .aider.conf.yml
        let aider_config = format!(
            "# mur-managed: auto-load learning context\nread:\n  - {}\n",
            mur_context_path.display()
        );
        std::fs::write(&aider_conf, aider_config)?;
        hooks_installed.push("Aider");
    }

    // ─── Step C10: Detect and print setup hints for file-based tools ─
    // Zed reads: .rules > .cursorrules > .windsurfrules > AGENTS.md (first match wins)
    // Junie reads: .junie/guidelines.md
    // Trae reads: .trae/rules/
    // These are project-level, so we just print hints
    let file_based_hints: Vec<(&str, &str)> = vec![
        ("Zed", "Add `See ~/.mur/context.md` to your AGENTS.md or .rules file"),
        ("Junie", "Add `See ~/.mur/context.md` to .junie/guidelines.md"),
        ("Trae", "Add `See ~/.mur/context.md` to .trae/rules/mur.md"),
        ("Cline/Roo", "Add `See ~/.mur/context.md` to .clinerules"),
        ("Windsurf", "Add `See ~/.mur/context.md` to .windsurfrules"),
    ];

    // ─── Step G: Interactive LLM/Embedding setup ─────────────────
    println!();
    println!("Model setup for pattern learning & semantic search:");
    println!("  1) Cloud — API keys required, best quality");
    println!("  2) Local — Ollama, free, runs on your machine");
    println!("  3) Skip — keep current config");
    print!("Choose [1/2/3] (default: 3): ");
    io::stdout().flush()?;
    let mut model_choice = String::new();
    io::stdin().read_line(&mut model_choice)?;
    let model_choice = model_choice.trim().to_string();

    match model_choice.as_str() {
        "1" => {
            // Cloud provider selection
            println!();
            println!("Cloud provider:");
            println!("  1) OpenRouter (recommended — access to many models)");
            println!("  2) OpenAI");
            println!("  3) Gemini");
            println!("  4) Anthropic");
            print!("Choose [1/2/3/4] (default: 1): ");
            io::stdout().flush()?;
            let mut provider_choice = String::new();
            io::stdin().read_line(&mut provider_choice)?;
            let provider_choice = provider_choice.trim().to_string();

            let (provider, llm_model, embed_model, env_var) = match provider_choice.as_str() {
                "2" => (
                    "openai",
                    "gpt-4o-mini",
                    "text-embedding-3-small",
                    "OPENAI_API_KEY",
                ),
                "3" => (
                    "gemini",
                    "gemini-2.0-flash",
                    "text-embedding-004",
                    "GEMINI_API_KEY",
                ),
                "4" => (
                    "anthropic",
                    "claude-sonnet-4-20250514",
                    "voyage-3-lite",
                    "ANTHROPIC_API_KEY",
                ),
                _ => (
                    "openai",
                    "google/gemini-2.5-flash",
                    "openai/text-embedding-3-small",
                    "OPENROUTER_API_KEY",
                ),
            };

            // Check for API key in environment
            if std::env::var(env_var).is_ok() {
                println!("  ✓ {} detected", env_var);
            } else {
                println!(
                    "  ⚠ {} not set — set it before using MUR learning features",
                    env_var
                );
            }

            // OpenRouter uses OpenAI-compatible API
            let is_openrouter = env_var == "OPENROUTER_API_KEY";
            let openai_url_line = if is_openrouter {
                "\n  openai_url: https://openrouter.ai/api/v1"
            } else {
                ""
            };
            let llm_openai_url_line = if is_openrouter {
                "\n    openai_url: https://openrouter.ai/api/v1"
            } else {
                ""
            };
            // For OpenRouter, embedding uses Ollama locally (free + fast)
            let (search_section, display_provider) = if is_openrouter {
                (
                    "search:\n  provider: ollama\n  model: qwen3-embedding".to_string(),
                    "openrouter (LLM) + ollama (search)",
                )
            } else {
                (
                    format!(
                        "search:\n  provider: {provider}\n  model: {embed_model}\n  api_key_env: {env_var}{openai_url_line}"
                    ),
                    provider,
                )
            };

            let config_content = format!(
                r#"# MUR Configuration
# See: https://github.com/mur-run/mur

tools:
  claude:
    enabled: true
  gemini:
    enabled: true

{search_section}

learning:
  llm:
    provider: {provider}
    model: {llm_model}
    api_key_env: {env_var}{llm_openai_url_line}
"#
            );
            std::fs::write(&config_path, config_content)?;
            println!("  ✓ Config updated: {} / {}", display_provider, llm_model);
        }
        "2" => {
            // Local (Ollama) setup
            let ollama_running = std::process::Command::new("ollama")
                .arg("list")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);

            if !ollama_running {
                println!();
                println!("  ⚠ Ollama not detected. Install from https://ollama.com");
                println!("  Using default Ollama models in config (pull them later).");
            } else {
                println!("  ✓ Ollama detected");
            }

            println!();
            println!("LLM model for pattern learning:");
            println!("  1) llama3.2:3b (default, lightweight)");
            println!("  2) llama3.1:8b (better quality)");
            println!("  3) qwen3:4b (good for code)");
            print!("Choose [1/2/3] (default: 1): ");
            io::stdout().flush()?;
            let mut llm_choice = String::new();
            io::stdin().read_line(&mut llm_choice)?;
            let llm_model = match llm_choice.trim() {
                "2" => "llama3.1:8b",
                "3" => "qwen3:4b",
                _ => "llama3.2:3b",
            };

            println!();
            println!("Embedding model for semantic search:");
            println!("  1) qwen3-embedding:0.6b (default, fast)");
            println!("  2) nomic-embed-text (good quality)");
            print!("Choose [1/2] (default: 1): ");
            io::stdout().flush()?;
            let mut embed_choice = String::new();
            io::stdin().read_line(&mut embed_choice)?;
            let embed_model = match embed_choice.trim() {
                "2" => "nomic-embed-text",
                _ => "qwen3-embedding:0.6b",
            };

            let config_content = format!(
                r#"# MUR Configuration
# See: https://github.com/mur-run/mur

tools:
  claude:
    enabled: true
  gemini:
    enabled: true

search:
  provider: ollama
  model: {embed_model}

learning:
  llm:
    provider: ollama
    model: {llm_model}
"#
            );
            std::fs::write(&config_path, config_content)?;
            println!("  ✓ Config updated: ollama / {}", llm_model);
        }
        _ => {
            // Skip — keep current config
            println!("  Keeping current config.");
        }
    }

    // ─── Step H: Community sharing opt-in ──────────────────────────
    println!();
    print!("Enable community pattern sharing? [y/N] ");
    io::stdout().flush()?;
    let mut community_answer = String::new();
    io::stdin().read_line(&mut community_answer)?;
    let community_enabled = {
        let a = community_answer.trim().to_lowercase();
        a == "y" || a == "yes"
    };

    if community_enabled {
        // Update config to enable community
        if let Ok(mut config) = store::config::load_config() {
            config.community.enabled = true;
            let _ = store::config::save_config(&config);
        }
        println!("  Community sharing enabled.");
        println!("  Run `mur login` to authenticate and start sharing patterns.");
    }

    // ─── Step D: Detect other tools ──────────────────────────────
    let gemini_settings = home.join(".gemini").join("settings.json");
    let cursor_rules = std::env::current_dir().ok().map(|d| d.join(".cursorrules"));

    let mut detected_tools = Vec::new();

    if gemini_settings.exists() || home.join(".gemini").exists() {
        detected_tools.push("Gemini CLI");
        // Antigravity uses Gemini under the hood — same hooks apply
        detected_tools.push("Antigravity");
    }
    if let Some(ref cr) = cursor_rules
        && cr.exists()
    {
        detected_tools.push("Cursor");
    }

    // Check for CLI-based AI tools via `which`
    let cli_tools = [
        ("codex", "Codex"),
        ("auggie", "Auggie"),
        ("aider", "Aider"),
        ("openclaw", "OpenClaw"),
        ("opencode", "OpenCode"),
        ("amp", "Amp"),
        ("zed", "Zed"),
    ];
    for (binary, name) in &cli_tools {
        if std::process::Command::new("which")
            .arg(binary)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            detected_tools.push(name);
        }
    }

    // Check for GitHub Copilot config directory
    if home.join(".config").join("github-copilot").exists() || home.join(".copilot").exists() {
        detected_tools.push("GitHub Copilot");
    }

    // Check for Cline/Roo (VS Code extension — detect .clinerules in cwd)
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(".clinerules").exists() || cwd.join(".roomodes").exists() {
            detected_tools.push("Cline/Roo");
        }
    }

    // Check for Windsurf
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(".windsurfrules").exists() || cwd.join(".windsurf").exists() {
            detected_tools.push("Windsurf");
        }
    }

    // Check for Amazon Q
    if home.join(".amazonq").exists() {
        detected_tools.push("Amazon Q");
    }

    // Check for JetBrains Junie
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join(".junie").exists() {
            detected_tools.push("Junie");
        }
        if cwd.join(".trae").exists() {
            detected_tools.push("Trae");
        }
    }

    // ─── Step F: Print summary ───────────────────────────────────
    let pattern_count = if mur_dir.join("patterns").exists() {
        std::fs::read_dir(mur_dir.join("patterns"))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "yaml" || ext == "yml")
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };

    println!();
    println!("✅ MUR initialized!");
    println!();
    println!("  📁 Data directory: ~/.mur/");
    if !hooks_installed.is_empty() {
        println!("  🪝 Hooks installed: {}", hooks_installed.join(", "));
    } else {
        println!("  🪝 Hooks: not installed (run `mur init --hooks` to install)");
    }
    println!(
        "  📝 Patterns: {} {}",
        pattern_count,
        if pattern_count == 0 {
            "(run `mur new` to create your first)"
        } else {
            ""
        }
    );

    // Show detected tools
    if !detected_tools.is_empty() {
        println!();
        println!("  🔍 Detected tools: {}", detected_tools.join(", "));
    }

    // Show file-based tool hints
    let show_hints: Vec<_> = file_based_hints.iter()
        .filter(|(tool, _)| detected_tools.contains(tool))
        .collect();
    if !show_hints.is_empty() {
        println!();
        println!("  📝 File-based tools (add MUR context manually):");
        for (tool, hint) in &show_hints {
            println!("    💡 {}: {}", tool, hint);
        }
    }

    println!();
    println!("  Next steps:");
    println!("    1. Start coding — MUR injects patterns automatically via hooks");
    println!("    2. Run `mur context --file` to update context for file-based tools");
    println!("    3. Run `mur search <query>` to find patterns");
    if community_enabled {
        println!("    4. Run `mur login` to authenticate for community sharing");
        println!("    5. Run `mur community list` to browse community patterns");
    }
    println!();

    Ok(())
}

// ─── Phase 0 + Phase 4 commands ─────────────────────────────────

async fn cmd_serve(port: u16, open: bool, readonly: bool) -> Result<()> {
    let mur_dir = dirs::home_dir().expect("no home dir").join(".mur");

    let (events_tx, _) = tokio::sync::broadcast::channel(64);
    let state = server::AppState {
        patterns_dir: mur_dir.join("patterns"),
        workflows_dir: mur_dir.join("workflows"),
        config: server::ServerConfig { readonly },
        events_tx,
    };

    let open_url = if open {
        Some(format!("http://localhost:{}", port))
    } else {
        None
    };

    server::run_server(state, port, open_url).await
}

fn cmd_why(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;
    interactive::explain_why(&pattern, &store)
}

fn cmd_edit(name: &str, quick: bool) -> Result<()> {
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
                    .with_prompt(&format!(
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
                    .with_prompt(&format!(
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
