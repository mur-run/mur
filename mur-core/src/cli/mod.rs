//! CLI entry point — `Cli` parser + top-level `Commands` enum. Sub-action
//! enums live in sibling modules. Pure clap derive types — no logic here.

pub mod actions;
pub mod agent;

pub use actions::*;
pub use agent::*;

use clap::{Parser, Subcommand};

use crate::cmd;

#[derive(Parser)]
#[command(name = "mur", version, about = "Continuous learning for AI assistants")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new pattern interactively
    New {
        /// Path to a diagram file to attach (mermaid, plantuml)
        #[arg(long)]
        diagram: Option<String>,
    },
    /// Search patterns + sources (unified).
    Search {
        /// Search query
        query: String,
        /// Filter to a specific source id (repeatable).
        #[arg(long)]
        source: Vec<String>,
        /// `patterns`, `sources`, or `all` (default).
        #[arg(long = "type", default_value = "all")]
        result_type: String,
        /// Shortcut: same as --type sources
        #[arg(long)]
        only_sources: bool,
        /// Shortcut: same as --type patterns
        #[arg(long)]
        only_patterns: bool,
        /// Max sources results.
        #[arg(long, short = 'k', default_value_t = 8)]
        limit: usize,
        /// JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Learn from sessions
    Learn {
        #[command(subcommand)]
        action: LearnAction,
    },
    /// Show statistics and effectiveness
    Stats,
    /// Check MUR setup and configuration health
    Doctor,
    /// Sync patterns to AI tools (or run a sync subcommand)
    Sync {
        /// Suppress output
        #[arg(long)]
        quiet: bool,
        /// Force project-aware sync (prioritize patterns matching project tags/language)
        #[arg(long)]
        project: bool,
        #[command(subcommand)]
        action: Option<SyncAction>,
    },
    /// Inject patterns for a query (hook integration)
    Inject {
        #[arg(long)]
        query: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Unified hook entry point for all AI tools (prompt/tool/stop/session-start)
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
    /// Manage the murmurd background daemon
    Murmurd {
        #[command(subcommand)]
        action: MurmurdAction,
    },
    /// Run a workflow by name or semantic query
    Run {
        /// Workflow name or search query
        query: String,
        /// Cancel remaining parallel branches on first failure
        #[arg(long)]
        fail_fast: bool,
        /// Print workflow as AI prompt instead of executing
        #[arg(long)]
        prompt: bool,
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
    Reindex {
        /// Initialise the versioned git store and commit all existing patterns
        /// in one bootstrap commit. Run once to enable `mur pattern history`.
        #[arg(long)]
        bootstrap: bool,
    },
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
        /// Bust the discovery cache and re-probe runtimes before init
        #[arg(long)]
        refresh_discovery: bool,
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
    /// Manage murmur agents (create, list, status, send, stop, ...)
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Manage shared model registry (~/.mur/models.yaml)
    Model(cmd::model::ModelArgs),
    /// Verify documentation claims (paths, commands, code refs) against actual codebase
    Verify {
        /// Specific file to verify (default: scan all docs)
        #[arg(long)]
        file: Option<String>,
        /// Show all claims (including valid and skipped)
        #[arg(long)]
        all: bool,
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
    /// Start session recording and inject context (shorthand for session start + context)
    In {
        /// Source identifier (e.g. claude-code)
        #[arg(long, default_value = "claude-code")]
        source: String,
    },
    /// Stop session recording with post-session menu (shorthand for session stop + next action)
    Out {
        /// Action to perform: analyze, export, skip (skips menu in non-TTY mode)
        #[arg(long)]
        action: Option<String>,
        /// Force LLM analysis even for short sessions
        #[arg(long)]
        force: bool,
    },
    /// Push pending signals from the outbox to the server
    Push {
        /// Preview what would be pushed without sending
        #[arg(long)]
        dry_run: bool,
    },
    /// Fetch pending signals from the server into the inbox
    Fetch {
        /// Preview what would be fetched without downloading
        #[arg(long)]
        dry_run: bool,
    },
    /// List / show / accept / reject pending pattern drafts (Channel 2/3 proposals).
    Drafts {
        #[command(subcommand)]
        action: DraftsAction,
    },
    /// Stop recording without export (alias: quit)
    Exit,
    /// Stop recording without export (alias: exit)
    Quit,
    /// Manage Docker Compose deployment
    Deploy {
        #[command(subcommand)]
        action: DeployAction,
    },
    /// Conversations archive (Layer 1 index, Layer 2 summary, Layer 3 raw)
    Chat {
        #[command(subcommand)]
        action: ChatAction,
    },
    /// Conversations archive management (pull / cleanup / reindex / doctor / preflight / migrate / rollback)
    Conversations {
        #[command(subcommand)]
        action: ConversationsAction,
    },
    /// Ask a natural-language question about your conversation archive (Mode C).
    Ask {
        /// Question to ask. Required unless --show-session is passed.
        question: Option<String>,
        /// Filter results to a specific source (e.g. "cc", "cursor").
        /// Phase 3.2.1: the source filter applies ONLY to day-level content
        /// (layers 0/1/2). Weekly/monthly rollup hits (layers 3/4) are
        /// multi-source aggregates and always surface based on relevance,
        /// regardless of --src.
        #[arg(long)]
        src: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long, default_value = "5")]
        k: usize,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        min_score: Option<f64>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        no_escalate: bool,
        #[arg(long)]
        debug_prompt: bool,
        #[arg(long)]
        strict_citations: bool,
        /// Append to the current session (multi-turn mode; Phase 3.3).
        #[arg(long = "continue", conflicts_with = "new_flag")]
        continue_flag: bool,
        /// Archive current session and start fresh (default; flag is explicit for scripts).
        #[arg(long = "new", conflicts_with = "continue_flag")]
        new_flag: bool,
        /// Print current session path, turn count, last turn time.
        /// Ignores question if given; no LLM calls.
        #[arg(long, conflicts_with_all = ["continue_flag", "new_flag"])]
        show_session: bool,
        /// Phase 3.5.1: Disable Stage 1b (LLM-abstractive hit compression)
        /// for this invocation. Overrides `conversations.ask.summarize_hits_enabled`.
        #[arg(long, conflicts_with = "summarize_model")]
        no_summarize: bool,
        /// Phase 3.5.1: Override the Stage 1b model for this invocation.
        /// Overrides `conversations.ask.summarize_model`; `None` (flag omitted)
        /// falls back to the config value, and ultimately to `ask.model`.
        #[arg(long)]
        summarize_model: Option<String>,
    },
    #[cfg(feature = "sources")]
    /// Manage external knowledge sources (Notion, Obsidian, Joplin, ...).
    Source {
        #[command(subcommand)]
        cmd: cmd::source_cmd::SourceCommand,
    },
    /// Low-level access to versioned-store internals (git repos, index rebuild)
    Internals {
        #[command(subcommand)]
        action: InternalsAction,
    },
}
