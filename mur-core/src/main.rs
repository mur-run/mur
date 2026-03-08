use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod auth;
mod capture;
mod cmd;
mod community;
mod context_api;
mod dashboard;
mod evolve;
mod gep;
mod inject;
mod interactive;
mod llm;
mod retrieve;
mod server;
mod session;
mod store;
mod team;

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
        /// Force project-aware sync (prioritize patterns matching project tags/language)
        #[arg(long)]
        project: bool,
    },
    /// Inject patterns for a query (hook integration)
    Inject {
        #[arg(long)]
        query: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Run a workflow by name or semantic query
    Run {
        /// Workflow name or search query
        query: String,
    },
    /// Report pattern feedback
    Feedback {
        #[command(subcommand)]
        action: FeedbackAction,
    },

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
        /// Run full consolidation (dedup, contradiction, promotion, decay, archival)
        #[arg(long)]
        consolidate: bool,
        #[command(subcommand)]
        action: Option<EvolveAction>,
    },
    /// Gene Evolution Protocol
    Gep {
        #[command(subcommand)]
        action: GepAction,
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
        /// Quiet mode — only output injected patterns, suppress evolution/stats
        #[arg(long, short)]
        quiet: bool,
        /// Compact output (shorter, fewer patterns)
        #[arg(long)]
        compact: bool,
        /// Override auto-detected query with explicit one
        #[arg(long)]
        query: Option<String>,
        /// Write context to ~/.mur/context.md for file-based tools (Aider, Cline, Windsurf)
        #[arg(long)]
        file: bool,
        /// Token budget (default: 2000)
        #[arg(long, default_value = "2000")]
        budget: usize,
        /// Source tool identifier (default: "cli")
        #[arg(long, default_value = "cli")]
        source: String,
        /// Output as JSON instead of formatted text
        #[arg(long)]
        json: bool,
        /// Scope filter (repeatable key=value, e.g. --scope user=david --scope project=mur)
        #[arg(long)]
        scope: Vec<String>,
    },
    /// Session recording for AI tool hooks
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Community publish/fetch
    Community {
        #[command(subcommand)]
        action: CommunityAction,
    },
    /// Team shared patterns
    Team {
        #[command(subcommand)]
        action: TeamAction,
    },
    /// Log in to mur community
    Login,
    /// Log out from mur community
    Logout,
    /// Initialize MUR directory and optionally install hooks
    Init {
        /// Install hooks for detected AI tools
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
    /// Import/export patterns in MKEF (MUR Knowledge Exchange Format)
    Exchange {
        #[command(subcommand)]
        action: ExchangeAction,
    },
    /// Analyze source files to detect coding conventions and generate a code-style pattern
    Analyze {
        /// Preview detected conventions without saving
        #[arg(long)]
        dry_run: bool,
    },
    /// Import rules from AI tool config files (.cursorrules, CLAUDE.md, etc.)
    Import {
        /// Files to import (auto-detects if not specified)
        #[arg(long)]
        file: Option<Vec<String>>,
        /// Preview what would be imported without saving
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ExchangeAction {
    /// Import a single MKEF file
    Import {
        /// Path to MKEF YAML file
        file: String,
    },
    /// Import all MKEF files from ~/.mur/exchange/
    ImportAll,
    /// Export a pattern to MKEF format
    Export {
        /// Pattern name to export
        name: String,
        /// Output directory (default: ~/.mur/exchange/)
        #[arg(long)]
        dir: Option<String>,
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
        /// Use LLM to analyze transcript and extract patterns
        #[arg(long)]
        llm: bool,
    },
    /// Analyze patterns across projects to find universal patterns
    Cross {
        /// Minimum number of projects a pattern must be used in for auto-promotion
        #[arg(long, default_value = "3")]
        min_projects: usize,
        /// Preview changes without saving
        #[arg(long)]
        dry_run: bool,
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
enum GepAction {
    /// Run one GEP evolution generation
    Evolve,
    /// Show population fitness statistics
    Status,
}

#[derive(Subcommand)]
enum EvolveAction {
    /// Show workflow composition suggestions from co-occurrence patterns
    Compose {
        /// Auto-create suggested workflows as drafts
        #[arg(long)]
        create: bool,
    },
    /// Show the pattern co-occurrence matrix
    Cooccurrence {
        /// Minimum count to display a pair
        #[arg(long, default_value = "2")]
        min: u32,
    },
}

#[derive(Subcommand)]
enum WorkflowAction {
    /// List all workflows
    List,
    /// Show a workflow by name
    Show {
        name: String,
        /// Output as markdown (optimized for AI consumption)
        #[arg(long)]
        md: bool,
    },
    /// Semantic search for workflows (uses LanceDB if available)
    Search {
        /// Search query
        query: String,
        /// Max results
        #[arg(long, default_value = "5")]
        limit: usize,
    },
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
    /// Open session review in the web dashboard
    Review {
        /// Session ID prefix
        id: String,
    },
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
    /// Report effectiveness of a community pattern
    Report {
        /// Pattern name or ID
        name: String,
        /// Effectiveness score (0.0-1.0)
        #[arg(long)]
        effectiveness: f64,
        /// Number of sessions used
        #[arg(long)]
        sessions: u32,
    },
    /// List available community packs
    Packs,
    /// View or install a community pack
    Pack {
        #[command(subcommand)]
        action: PackAction,
    },
}

#[derive(Subcommand)]
enum TeamAction {
    /// List team patterns
    List {
        /// Team ID
        #[arg(long, env = "MUR_TEAM_ID")]
        team: String,
    },
    /// Share a pattern to your team
    Share {
        /// Pattern name
        name: String,
        /// Team ID
        #[arg(long, env = "MUR_TEAM_ID")]
        team: String,
    },
    /// Pull latest team patterns
    Sync {
        /// Team ID
        #[arg(long, env = "MUR_TEAM_ID")]
        team: String,
    },
}

#[derive(Subcommand)]
enum PackAction {
    /// Install a community pack (downloads all its patterns)
    Install {
        /// Pack ID
        id: String,
    },
    /// Show details of a community pack
    Show {
        /// Pack ID
        id: String,
    },
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
        Commands::New { diagram } => cmd::pattern::cmd_new(diagram)?,
        Commands::Search { query } => cmd::pattern::cmd_search(&query)?,
        Commands::Stats => cmd::misc::cmd_stats()?,
        Commands::Pin { name } => cmd::pattern::cmd_set_lifecycle(&name, "pin")?,
        Commands::Mute { name } => cmd::pattern::cmd_set_lifecycle(&name, "mute")?,
        Commands::Boost { name, amount } => cmd::pattern::cmd_boost(&name, amount)?,
        Commands::Feedback { action } => match action {
            FeedbackAction::Helpful { name } => cmd::pattern::cmd_feedback(&name, true)?,
            FeedbackAction::Unhelpful { name } => cmd::pattern::cmd_feedback(&name, false)?,
            FeedbackAction::Auto { file, dry_run } => {
                cmd::pattern::cmd_feedback_auto(file, dry_run)?
            }
        },
        Commands::Gc { auto } => cmd::misc::cmd_gc(auto)?,

        Commands::Learn { action } => match action {
            LearnAction::Extract {
                file,
                fingerprint,
                llm,
            } => {
                cmd::learn::cmd_learn_extract(file, fingerprint, llm).await?;
            }
            LearnAction::Cross {
                min_projects,
                dry_run,
            } => {
                cmd::learn::cmd_learn_cross(min_projects, dry_run)?;
            }
        },
        Commands::Sync { quiet, project } => cmd::sync_cmd::cmd_sync(quiet, project)?,
        Commands::Inject { query, project: _ } => cmd::inject_cmd::cmd_inject(&query).await?,
        Commands::Run { query } => cmd::workflow::cmd_workflow_run(&query).await?,
        Commands::Pattern { action } => match action {
            PatternAction::Show { name } => cmd::pattern::cmd_pattern_show(&name)?,
        },
        Commands::Workflow { action } => match action {
            WorkflowAction::List => cmd::workflow::cmd_workflow_list()?,
            WorkflowAction::Show { name, md } => cmd::workflow::cmd_workflow_show(&name, md)?,
            WorkflowAction::Search { query, limit } => {
                cmd::workflow::cmd_workflow_search(&query, limit).await?
            }
            WorkflowAction::New => cmd::workflow::cmd_workflow_new()?,
        },
        Commands::Reindex => cmd::reindex::cmd_reindex().await?,
        Commands::Promote { name, tier } => cmd::pattern::cmd_promote(&name, &tier)?,
        Commands::Deprecate { name } => cmd::pattern::cmd_deprecate(&name)?,
        Commands::Links { name } => cmd::pattern::cmd_links(&name)?,
        Commands::Evolve {
            dry_run,
            force,
            consolidate,
            action,
        } => {
            if let Some(action) = action {
                match action {
                    EvolveAction::Compose { create } => {
                        cmd::evolve_cmd::cmd_evolve_compose(create)?
                    }
                    EvolveAction::Cooccurrence { min } => {
                        cmd::evolve_cmd::cmd_evolve_cooccurrence(min)?
                    }
                }
            } else if consolidate {
                cmd::evolve_cmd::cmd_consolidate(dry_run)?;
            } else {
                cmd::evolve_cmd::cmd_evolve(dry_run, force)?;
            }
        }
        Commands::Gep { action } => match action {
            GepAction::Evolve => cmd::community_cmd::cmd_gep_evolve()?,
            GepAction::Status => cmd::community_cmd::cmd_gep_status()?,
        },
        Commands::Emerge { threshold, dry_run } => cmd::learn::cmd_emerge(threshold, dry_run)?,
        Commands::Suggest { create } => cmd::workflow::cmd_suggest(create)?,
        Commands::Context {
            quiet,
            compact,
            query,
            file,
            budget,
            source,
            json,
            scope,
        } => {
            cmd::context::cmd_context(query, compact, file, budget, source, json, scope, quiet)
                .await?
        }
        Commands::Session { action } => match action {
            SessionAction::Start { source } => cmd::session::cmd_session_start(&source)?,
            SessionAction::Stop { analyze } => cmd::session::cmd_session_stop(analyze)?,
            SessionAction::Record {
                event_type,
                tool,
                content,
            } => cmd::session::cmd_session_record(&event_type, tool.as_deref(), &content)?,
            SessionAction::Status => cmd::session::cmd_session_status()?,
            SessionAction::List => cmd::session::cmd_session_list()?,
            SessionAction::Review { id } => cmd::session::cmd_session_review(&id)?,
        },
        Commands::Dashboard => {
            dashboard::render_dashboard()?;
        }
        Commands::Community { action } => match action {
            CommunityAction::Publish { name } => {
                cmd::community_cmd::cmd_community_publish(&name).await?
            }
            CommunityAction::Fetch { id } => cmd::community_cmd::cmd_community_fetch(&id).await?,
            CommunityAction::Search { query } => {
                cmd::community_cmd::cmd_community_search(&query).await?
            }
            CommunityAction::List { sort } => cmd::community_cmd::cmd_community_list(&sort).await?,
            CommunityAction::Star { id } => cmd::community_cmd::cmd_community_star(&id).await?,
            CommunityAction::Report {
                name,
                effectiveness,
                sessions,
            } => cmd::community_cmd::cmd_community_report(&name, effectiveness, sessions).await?,
            CommunityAction::Packs => cmd::community_cmd::cmd_community_packs().await?,
            CommunityAction::Pack { action } => match action {
                PackAction::Install { id } => {
                    cmd::community_cmd::cmd_community_pack_install(&id).await?
                }
                PackAction::Show { id } => cmd::community_cmd::cmd_community_pack_show(&id).await?,
            },
        },
        Commands::Team { action } => match action {
            TeamAction::List { team } => cmd::community_cmd::cmd_team_list(&team).await?,
            TeamAction::Share { name, team } => {
                cmd::community_cmd::cmd_team_share(&name, &team).await?
            }
            TeamAction::Sync { team } => cmd::community_cmd::cmd_team_sync(&team).await?,
        },
        Commands::Login => cmd::misc::cmd_login().await?,
        Commands::Logout => cmd::misc::cmd_logout()?,
        Commands::Init { hooks } => cmd::init::cmd_init(hooks)?,
        Commands::Serve {
            port,
            open,
            readonly,
        } => cmd::server_cmd::cmd_serve(port, open, readonly).await?,
        Commands::Why { name } => cmd::inject_cmd::cmd_why(&name)?,
        Commands::Edit { name, quick } => cmd::pattern::cmd_edit(&name, quick)?,
        Commands::Exchange { action } => match action {
            ExchangeAction::Import { file } => cmd::misc::cmd_exchange_import(&file)?,
            ExchangeAction::ImportAll => cmd::misc::cmd_exchange_import_all()?,
            ExchangeAction::Export { name, dir } => cmd::misc::cmd_exchange_export(&name, dir)?,
        },
        Commands::Analyze { dry_run } => cmd::misc::cmd_analyze(dry_run)?,
        Commands::Import { file, dry_run } => cmd::misc::cmd_import(file, dry_run)?,
    }

    Ok(())
}
