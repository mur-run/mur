use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod auth;
mod capture;
mod cmd;
mod community;
mod context_api;
mod conversations;
mod dashboard;
mod evolve;
mod executor;
mod extract;
mod extract_llm;
mod gep;
mod inject;
mod interactive;
mod llm;
mod paths;
mod retrieve;
mod server;
#[cfg(feature = "server")]
mod server_agents;
mod session;
#[cfg(feature = "sources")]
mod sources;
mod store;
mod sync;
mod team;
mod verify;

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
    /// Manage murmur agents (create, list, status, send, stop, ...)
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
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
    /// Manage workflow schedules
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
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
    /// Publish a workflow to a team
    Publish {
        /// Workflow name
        name: String,
        /// Team slug
        #[arg(long)]
        team: String,
    },
    /// Install a workflow from a team
    Install {
        /// Workflow name
        name: String,
        /// Team slug to install from
        #[arg(long)]
        from: String,
    },
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// List all scheduled workflows
    List,
    /// Set a cron schedule on a workflow
    Set {
        /// Workflow name
        name: String,
        /// Cron expression (e.g. "0 * * * *" for hourly)
        cron: String,
    },
    /// Remove the schedule from a workflow
    Remove {
        /// Workflow name
        name: String,
    },
    /// Enable a disabled schedule
    Enable {
        /// Workflow name
        name: String,
    },
    /// Disable a schedule without removing it
    Disable {
        /// Workflow name
        name: String,
    },
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
    /// Show session details and events
    Show {
        /// Session ID or prefix
        id: String,
        /// Show only the last N events
        #[arg(long)]
        last: Option<usize>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Export a session recording
    Export {
        /// Session ID or prefix
        id: String,
        /// Export format: json, markdown, skill
        #[arg(long, default_value = "markdown")]
        format: String,
        /// Run analysis/fingerprint extraction
        #[arg(long)]
        analyze: bool,
        /// Output file (defaults to stdout)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Push session recording(s) to the cloud server
    Push {
        /// Session ID or prefix (pushes most recent if omitted)
        id: Option<String>,
        /// Push all unsynced sessions
        #[arg(long)]
        all: bool,
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

#[derive(Subcommand)]
enum SyncAction {
    /// Show sync status (outbox/inbox queue depths, last fetch time)
    Status,
}

#[derive(Subcommand)]
enum ChatAction {
    /// List days in the archive (Layer 1)
    List {
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        src: Option<String>,
    },
    /// Show a single day's summary (or raw if no summary) (Layer 2)
    Show { date: String },
    /// Dump raw JSONL for a conversation (Layer 3)
    Raw { date: String, conv: String },
    /// Semantic + keyword search
    Search {
        query: String,
        #[arg(long, default_value = "10")]
        limit: usize,
        #[arg(long)]
        src: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConversationsAction {
    /// Run polling ingesters (Cursor/Gemini/Aider)
    Pull,
    /// Apply retention cleanup
    Cleanup,
    /// Rebuild LanceDB from raw + summaries.
    Reindex {
        /// Skip span (layer=2) rebuild; only re-ingest raw → layer=0.
        #[arg(long, conflicts_with_all = ["spans_only", "rollups_only"])]
        raw_only: bool,
        /// Skip raw rebuild; only re-process summary/*.md → layer=2.
        #[arg(long, conflicts_with_all = ["raw_only", "rollups_only"])]
        spans_only: bool,
        /// Only re-process summary/weekly/*.md + summary/monthly/*.md → layer=3/4.
        #[arg(long, conflicts_with_all = ["raw_only", "spans_only"])]
        rollups_only: bool,
    },
    /// Run health checks
    Doctor,
    /// Check migration preconditions (BP1)
    Preflight,
    /// Migrate from commander paths (BP2: dry-run by default; BP3: recovery flags)
    Migrate {
        /// Actually perform the migration (default: dry-run only, no changes)
        #[arg(long)]
        run: bool,
        /// Resume from a previously interrupted migration
        #[arg(long, conflicts_with_all = &["run", "discard_staging"])]
        resume: bool,
        /// Discard any staging dir from a previously interrupted migration
        #[arg(long, conflicts_with_all = &["run", "resume"])]
        discard_staging: bool,
    },
    /// Roll back to commander's old paths
    Rollback,
    /// Generate hybrid summaries for completed days (sleep-time compact).
    Compact {
        /// One specific date (otherwise process all missing completed days).
        #[arg(long)]
        date: Option<String>,

        /// Lower bound for the sweep (ignored with --date).
        #[arg(long)]
        since: Option<String>,

        /// Overwrite existing summaries. Archives old version to .history/.
        #[arg(long)]
        force: bool,

        /// Only regenerate when raw content hash changed (implies force).
        #[arg(long)]
        if_stale: bool,

        /// Override throttle (default: config.compact.max_days_per_run).
        #[arg(long)]
        max_days: Option<u32>,

        /// Don't call Ollama — emit extractive-only skeleton (for testing).
        #[arg(long)]
        extractive_only: bool,

        /// Emit the LLM prompts to stderr without sending them.
        #[arg(long)]
        debug_prompt: bool,

        /// Skip the rollup cascade after day compact (Phase 3.2).
        #[arg(long)]
        skip_rollups: bool,
    },
    /// Generate weekly + monthly rollup summaries (Phase 3.2).
    Rollup {
        /// Specific ISO week to rollup (e.g. "2026-W16").
        #[arg(long)]
        week: Option<String>,
        /// Specific month to rollup (e.g. "2026-04").
        #[arg(long, conflicts_with = "week")]
        month: Option<String>,
        /// Sweep mode: rollup all missing weeks AND months.
        #[arg(long, conflicts_with_all = ["week", "month"])]
        all_missing: bool,
        /// Overwrite existing rollup; archive prior to .history/.
        #[arg(long)]
        force: bool,
        /// Phase 3.2.1: no-op retained for backward compatibility. The
        /// default (omitting --force) already regenerates only when the
        /// source content hash has changed via the internal idempotency
        /// check. Use --force to regenerate unconditionally.
        #[arg(long)]
        if_stale: bool,
        /// Override throttle for --all-missing.
        #[arg(long)]
        max_weeks: Option<u32>,
        /// Override throttle for --all-missing.
        #[arg(long)]
        max_months: Option<u32>,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// Create a new agent profile
    Create {
        /// Agent name (alphanumeric + underscore)
        name: String,
        /// Skip interactive prompts; use flag defaults
        #[arg(long)]
        no_interactive: bool,
        /// Display name (defaults to the agent name)
        #[arg(long)]
        display_name: Option<String>,
        /// Model id (e.g. llama3.2:3b, claude-opus-4-7).
        /// May also be `provider/model` (e.g. anthropic/claude-opus-4-7).
        #[arg(long)]
        model: Option<String>,
        /// Model provider (e.g. ollama, anthropic, openai). Defaults to ollama.
        /// Takes precedence over a `provider/model` prefix in --model.
        #[arg(long)]
        provider: Option<String>,
    },
    /// List agents under $MUR_HOME/agents
    List {
        /// Emit JSON instead of a human-readable table
        #[arg(long)]
        json: bool,
    },
    /// Show systemctl-style status for one agent
    Status {
        /// Agent name
        name: String,
    },
    /// Stop a running agent (SIGTERM, then SIGKILL after timeout)
    Stop {
        /// Agent name
        name: String,
    },
    /// Remove an agent (symlink + optionally data)
    Remove {
        /// Agent name
        name: String,
        /// Also delete the agent's data directory
        #[arg(long)]
        purge: bool,
    },
    /// Rename an agent (updates dir, profile.name, and symlink)
    Rename {
        /// Old name
        old: String,
        /// New name
        new: String,
    },
    /// Dial an agent's Unix socket and issue A2A `message/send`
    Send {
        /// Agent name
        name: String,
        /// A2A message JSON: {"role":"user","parts":[...]}
        message: String,
    },
    /// Dial an agent's Unix socket and print its A2A Agent Card
    Card {
        /// Agent name
        name: String,
    },
    /// Rotate an agent's Ed25519 identity keypair (P0a.6).
    Rekey {
        /// Agent name
        name: String,
        /// Why this rotation is happening (audit hint)
        #[arg(long, default_value = "scheduled")]
        reason: String,
        /// Skip the interactive confirmation prompt
        #[arg(long)]
        yes: bool,
        /// EMERGENCY rotation (old key not required). Writes an UNSIGNED
        /// attestation; peers refuse trust until commander admin approves.
        #[arg(long)]
        emergency: bool,
    },
    /// Show identity rotation status for an agent (P0a.6).
    RekeyStatus {
        /// Agent name
        name: String,
        /// Emit JSON instead of a human-readable table
        #[arg(long)]
        json: bool,
    },
    /// Generate and (optionally) install a launchd/systemd user service
    InstallService {
        /// Agent name
        name: String,
        /// Print the template only; do not write files or invoke launchctl/systemctl
        #[arg(long)]
        dry_run: bool,
    },
    /// Manage an agent's system prompt (show/edit/set)
    Prompt {
        #[command(subcommand)]
        action: AgentPromptAction,
    },
    /// Manage an agent's MCP servers (add/list/remove/rename)
    Mcp {
        #[command(subcommand)]
        action: AgentMcpAction,
    },
    /// Manage an agent's skills (add/list/remove/show)
    Skill {
        #[command(subcommand)]
        action: AgentSkillAction,
    },
    /// Manage an agent's permissions / entitlements
    Perm {
        #[command(subcommand)]
        action: AgentPermAction,
    },
    /// Export an agent to a .murpkg or self-contained binary
    Export {
        /// Agent name
        name: String,
        /// Output path (e.g. agent.murpkg or my_agent)
        #[arg(long, short = 'o')]
        out: String,
        /// Format: "pkg" (default) or "bin"
        #[arg(long, default_value = "pkg")]
        format: String,
    },
    /// Aggregate telemetry counters from <agent_home>/telemetry/*.jsonl
    Stats {
        /// Agent name
        name: String,
    },
    /// Print recent lines from the agent's stderr.log
    Logs {
        /// Agent name
        name: String,
        /// Show only the last N lines (default 20)
        #[arg(long, default_value_t = 20)]
        tail: usize,
    },
    /// Manage companion (warm voice + optional proactive messaging).
    Companion(cmd::agent_companion::CompanionArgs),
}

#[derive(Subcommand)]
enum AgentPermAction {
    /// Print the entitlements block (or one section, e.g. "network")
    Show {
        name: String,
        section: Option<String>,
    },
    /// Set a mode (currently only `network.outbound <restricted|unrestricted|off>`)
    SetMode {
        name: String,
        key: String,
        value: String,
    },
    /// Allow an outbound host glob
    AllowHost { name: String, glob: String },
    /// Deny an outbound host glob
    DenyHost { name: String, glob: String },
    /// Print the outbound host allow / deny lists
    ListHosts { name: String },
    /// Allow filesystem read on a path
    AllowRead { name: String, path: String },
    /// Allow filesystem write on a path
    AllowWrite { name: String, path: String },
    /// Deny filesystem access on a path
    DenyPath { name: String, path: String },
    /// Add a binary to the spawn allowlist
    AllowSpawn { name: String, binary: String },
    /// Remove a binary from the spawn allowlist
    DenySpawn { name: String, binary: String },
    /// Set a numeric resource limit (memory_mb, file_descriptors, processes)
    SetLimit {
        name: String,
        key: String,
        value: u64,
    },
}

#[derive(Subcommand)]
enum AgentSkillAction {
    /// List attached skill ids
    List { name: String },
    /// Copy a skill markdown file into the agent and register it.
    /// The skill is stored under `skills/<basename>` inside the agent dir.
    Add {
        name: String,
        /// Path to a skill .md file. Stored id is `skills/<basename>`.
        source: String,
    },
    /// Remove a skill entry; deletes the backing file if orphaned.
    /// `skill_id` may be the full id (`skills/foo.md`), the basename (`foo.md`),
    /// or the basename without extension (`foo`).
    Remove { name: String, skill_id: String },
    /// Print the contents of a skill file.
    /// `skill_id` may be the full id (`skills/foo.md`), the basename (`foo.md`),
    /// or the basename without extension (`foo`).
    Show { name: String, skill_id: String },
}

#[derive(Subcommand)]
enum AgentMcpAction {
    /// List MCP servers attached to an agent
    List { name: String },
    /// Add an MCP server entry and sync its command into the spawn allowlist
    Add {
        name: String,
        server_id: String,
        #[arg(long)]
        command: String,
        #[arg(long = "arg")]
        args: Vec<String>,
    },
    /// Remove an MCP server entry by id
    Remove { name: String, server_id: String },
    /// Rename an MCP server entry in place
    Rename {
        name: String,
        old: String,
        new: String,
    },
}

#[derive(Subcommand)]
enum AgentPromptAction {
    /// Print the agent's sys_prompt.md contents to stdout
    Show { name: String },
    /// Open $EDITOR on sys_prompt.md
    Edit { name: String },
    /// Write sys_prompt.md, preserving a .bak copy of the previous value
    Set {
        name: String,
        /// Inline prompt text (omit when using -f)
        content: Option<String>,
        /// Read prompt text from a file
        #[arg(short = 'f', long = "file")]
        file: Option<String>,
    },
}

#[derive(Subcommand)]
enum DraftsAction {
    /// List pending pattern drafts in a compact table.
    List {
        /// Only include drafts created within the last N days (default 30).
        #[arg(long, default_value_t = 30)]
        since: u32,
    },
    /// Show the full YAML + metadata for a single draft by id-prefix.
    Show {
        /// Unambiguous prefix of the draft's uuid (e.g. first 8 chars).
        id: String,
    },
    /// Accept a draft locally: saves the embedded Pattern to ~/.mur/patterns/
    /// with maturity=emerging. Does NOT yet notify the server (MVP).
    Accept {
        /// Unambiguous prefix of the draft's uuid.
        id: String,
        /// Override tier: session | project | core.
        #[arg(long = "as-tier")]
        as_tier: Option<String>,
    },
    /// Reject a draft server-side with an optional reason.
    Reject {
        /// Unambiguous prefix of the draft's uuid.
        id: String,
        /// Optional human-readable reason recorded on the draft.
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
enum DeployAction {
    /// Start services (docker compose up)
    Up {
        /// Rebuild images before starting
        #[arg(long)]
        build: bool,
        /// Run in the background (detached mode)
        #[arg(short, long)]
        detach: bool,
        /// Path to a docker-compose file (default: docker-compose.yml in cwd)
        #[arg(short, long)]
        file: Option<String>,
    },
    /// Stop and remove services (docker compose down)
    Down {
        /// Also remove named volumes declared in the compose file
        #[arg(long)]
        volumes: bool,
        /// Path to a docker-compose file
        #[arg(short, long)]
        file: Option<String>,
    },
    /// Show service status (docker compose ps)
    Status {
        /// Path to a docker-compose file
        #[arg(short, long)]
        file: Option<String>,
    },
    /// Show service logs (docker compose logs)
    Logs {
        /// Service name (shows all services if omitted)
        service: Option<String>,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
        /// Path to a docker-compose file
        #[arg(short, long)]
        file: Option<String>,
    },
    /// Build or rebuild service images (docker compose build)
    Build {
        /// Path to a docker-compose file
        #[arg(short, long)]
        file: Option<String>,
    },
}

/// Load .env file from `~/.mur/.env` (preferred), `~/.mur/commander/.env`, or `./.env` (fallback).
fn load_dotenv() {
    let home = dirs::home_dir();
    // Try ~/.mur/.env first
    if let Some(path) = home.as_ref().map(|h| h.join(".mur").join(".env"))
        && path.exists()
        && dotenvy::from_path(&path).is_ok()
    {
        return;
    }
    // Try ~/.mur/commander/.env (Commander shares API keys here)
    if let Some(path) = home
        .as_ref()
        .map(|h| h.join(".mur").join("commander").join(".env"))
        && path.exists()
        && dotenvy::from_path(&path).is_ok()
    {
        return;
    }
    // Fallback: current directory .env
    let _ = dotenvy::dotenv();
}

/// Windows has a 1 MB default main-thread stack, vs 8 MB on Unix. Our `async fn
/// async_main` Future is large (many subcommand handler states combined via
/// `match cli.command`), and blew the stack on Windows CI. Move the runtime
/// onto a worker thread with an explicit 8 MB stack to match Unix behavior.
fn main() -> Result<()> {
    std::thread::Builder::new()
        .name("mur-main".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build tokio runtime")
                .block_on(async_main())
        })
        .expect("spawn main worker thread")
        .join()
        .expect("mur-main thread panicked")
}

async fn async_main() -> Result<()> {
    load_dotenv();
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
        Commands::Search {
            query,
            source,
            result_type,
            only_sources,
            only_patterns,
            limit,
            json,
        } => {
            cmd_search_unified(
                query,
                source,
                result_type,
                only_sources,
                only_patterns,
                limit,
                json,
            )
            .await?
        }
        Commands::Stats => cmd::misc::cmd_stats()?,
        Commands::Doctor => cmd::misc::cmd_doctor()?,
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
        Commands::Sync {
            quiet,
            project,
            action,
        } => {
            if let Some(action) = action {
                match action {
                    SyncAction::Status => cmd::sync_cmd::run_status()?,
                }
            } else {
                cmd::sync_cmd::cmd_sync(quiet, project).await?;
            }
        }
        Commands::Inject { query, project: _ } => cmd::inject_cmd::cmd_inject(&query).await?,
        Commands::Run {
            query,
            fail_fast,
            prompt,
        } => cmd::workflow::cmd_workflow_run(&query, fail_fast, prompt).await?,
        Commands::Pattern { action } => match action {
            PatternAction::Show { name } => cmd::pattern::cmd_pattern_show(&name)?,
        },
        Commands::Workflow { action } => match action {
            WorkflowAction::List => cmd::workflow::cmd_workflow_list()?,
            WorkflowAction::Schedule { action } => match action {
                ScheduleAction::List => cmd::workflow::cmd_schedule_list()?,
                ScheduleAction::Set { name, cron } => {
                    cmd::workflow::cmd_schedule_set(&name, &cron)?
                }
                ScheduleAction::Remove { name } => cmd::workflow::cmd_schedule_remove(&name)?,
                ScheduleAction::Enable { name } => cmd::workflow::cmd_schedule_enable(&name, true)?,
                ScheduleAction::Disable { name } => {
                    cmd::workflow::cmd_schedule_enable(&name, false)?
                }
            },
            WorkflowAction::Show { name, md } => cmd::workflow::cmd_workflow_show(&name, md)?,
            WorkflowAction::Search { query, limit } => {
                cmd::workflow::cmd_workflow_search(&query, limit).await?
            }
            WorkflowAction::New => cmd::workflow::cmd_workflow_new()?,
            WorkflowAction::Publish { name, team } => {
                cmd::workflow::cmd_workflow_publish(&name, &team)?
            }
            WorkflowAction::Install { name, from } => {
                cmd::workflow::cmd_workflow_install(&name, &from)?
            }
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
            SessionAction::Stop { analyze } => cmd::session::cmd_session_stop(analyze).await?,
            SessionAction::Record {
                event_type,
                tool,
                content,
            } => cmd::session::cmd_session_record(&event_type, tool.as_deref(), &content)?,
            SessionAction::Status => cmd::session::cmd_session_status()?,
            SessionAction::List => cmd::session::cmd_session_list()?,
            SessionAction::Review { id } => cmd::session::cmd_session_review(&id)?,
            SessionAction::Show { id, last, json } => {
                cmd::session::cmd_session_show(&id, last, json)?
            }
            SessionAction::Export {
                id,
                format,
                analyze,
                output,
            } => cmd::session::cmd_session_export(&id, &format, analyze, output).await?,
            SessionAction::Push { id, all } => {
                cmd::session::cmd_session_push(id.as_deref(), all).await?
            }
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
        Commands::Agent { action } => match action {
            AgentAction::Create {
                name,
                no_interactive,
                display_name,
                model,
                provider,
            } => cmd::agent::cmd_create(&name, no_interactive, display_name, model, provider)?,
            AgentAction::List { json } => cmd::agent::cmd_list(json)?,
            AgentAction::Status { name } => cmd::agent::cmd_status(&name)?,
            AgentAction::Stop { name } => cmd::agent::cmd_stop(&name)?,
            AgentAction::Remove { name, purge } => cmd::agent::cmd_remove(&name, purge)?,
            AgentAction::Rename { old, new } => cmd::agent::cmd_rename(&old, &new)?,
            AgentAction::Send { name, message } => cmd::agent::cmd_send(&name, &message)?,
            AgentAction::Card { name } => cmd::agent::cmd_card(&name)?,
            AgentAction::Rekey {
                name,
                reason,
                yes,
                emergency,
            } => cmd::agent_rekey::cmd_rekey(&name, &reason, yes, emergency)?,
            AgentAction::RekeyStatus { name, json } => {
                cmd::agent_rekey::cmd_rekey_status(&name, json)?
            }
            AgentAction::InstallService { name, dry_run } => {
                cmd::agent::cmd_install_service(&name, dry_run)?
            }
            AgentAction::Prompt { action } => match action {
                AgentPromptAction::Show { name } => cmd::agent::cmd_prompt_show(&name)?,
                AgentPromptAction::Edit { name } => cmd::agent::cmd_prompt_edit(&name)?,
                AgentPromptAction::Set {
                    name,
                    content,
                    file,
                } => cmd::agent::cmd_prompt_set(&name, content.as_deref(), file.as_deref())?,
            },
            AgentAction::Mcp { action } => match action {
                AgentMcpAction::List { name } => cmd::agent::cmd_mcp_list(&name)?,
                AgentMcpAction::Add {
                    name,
                    server_id,
                    command,
                    args,
                } => cmd::agent::cmd_mcp_add(&name, &server_id, &command, &args)?,
                AgentMcpAction::Remove { name, server_id } => {
                    cmd::agent::cmd_mcp_remove(&name, &server_id)?
                }
                AgentMcpAction::Rename { name, old, new } => {
                    cmd::agent::cmd_mcp_rename(&name, &old, &new)?
                }
            },
            AgentAction::Skill { action } => match action {
                AgentSkillAction::List { name } => cmd::agent::cmd_skill_list(&name)?,
                AgentSkillAction::Add { name, source } => {
                    cmd::agent::cmd_skill_add(&name, &source)?
                }
                AgentSkillAction::Remove { name, skill_id } => {
                    cmd::agent::cmd_skill_remove(&name, &skill_id)?
                }
                AgentSkillAction::Show { name, skill_id } => {
                    cmd::agent::cmd_skill_show(&name, &skill_id)?
                }
            },
            AgentAction::Perm { action } => match action {
                AgentPermAction::Show { name, section } => {
                    cmd::agent::cmd_perm_show(&name, section.as_deref())?
                }
                AgentPermAction::SetMode { name, key, value } => {
                    cmd::agent::cmd_perm_set_mode(&name, &key, &value)?
                }
                AgentPermAction::AllowHost { name, glob } => {
                    cmd::agent::cmd_perm_allow_host(&name, &glob)?
                }
                AgentPermAction::DenyHost { name, glob } => {
                    cmd::agent::cmd_perm_deny_host(&name, &glob)?
                }
                AgentPermAction::ListHosts { name } => cmd::agent::cmd_perm_list_hosts(&name)?,
                AgentPermAction::AllowRead { name, path } => {
                    cmd::agent::cmd_perm_allow_read(&name, &path)?
                }
                AgentPermAction::AllowWrite { name, path } => {
                    cmd::agent::cmd_perm_allow_write(&name, &path)?
                }
                AgentPermAction::DenyPath { name, path } => {
                    cmd::agent::cmd_perm_deny_path(&name, &path)?
                }
                AgentPermAction::AllowSpawn { name, binary } => {
                    cmd::agent::cmd_perm_allow_spawn(&name, &binary)?
                }
                AgentPermAction::DenySpawn { name, binary } => {
                    cmd::agent::cmd_perm_deny_spawn(&name, &binary)?
                }
                AgentPermAction::SetLimit { name, key, value } => {
                    cmd::agent::cmd_perm_set_limit(&name, &key, value)?
                }
            },
            AgentAction::Export { name, out, format } => {
                cmd::agent::cmd_export(&name, &out, &format)?
            }
            AgentAction::Stats { name } => cmd::agent::cmd_stats(&name)?,
            AgentAction::Logs { name, tail } => cmd::agent::cmd_logs(&name, tail)?,
            AgentAction::Companion(args) => cmd::agent_companion::run(args).await?,
        },
        Commands::Exchange { action } => match action {
            ExchangeAction::Import { file } => cmd::misc::cmd_exchange_import(&file)?,
            ExchangeAction::ImportAll => cmd::misc::cmd_exchange_import_all()?,
            ExchangeAction::Export { name, dir } => cmd::misc::cmd_exchange_export(&name, dir)?,
        },
        Commands::Verify { file, all } => {
            // Initialize known commands from the clap tree so verify doesn't
            // need a hardcoded list.
            let clap_cmd = Cli::command();
            let known = verify::collect_commands_from_clap(&clap_cmd);
            verify::set_known_commands(known);
            cmd::verify::cmd_verify(file.as_deref(), all)?
        }
        Commands::Import { file, dry_run } => cmd::misc::cmd_import(file, dry_run)?,
        Commands::In { source } => cmd::session::cmd_in(&source).await?,
        Commands::Out { action, force } => cmd::session::cmd_out(action.as_deref(), force).await?,
        Commands::Push { dry_run } => {
            let config = crate::store::config::load_config()?;
            cmd::sync_cmd::run_push(&config.server.url, dry_run).await?;
        }
        Commands::Fetch { dry_run } => {
            let config = crate::store::config::load_config()?;
            cmd::sync_cmd::run_fetch(&config.server.url, dry_run).await?;
        }
        Commands::Drafts { action } => match action {
            DraftsAction::List { since } => cmd::drafts::cmd_drafts_list(since).await?,
            DraftsAction::Show { id } => cmd::drafts::cmd_drafts_show(&id).await?,
            DraftsAction::Accept { id, as_tier } => {
                cmd::drafts::cmd_drafts_accept(&id, as_tier.as_deref()).await?
            }
            DraftsAction::Reject { id, reason } => {
                cmd::drafts::cmd_drafts_reject(&id, reason.as_deref()).await?
            }
        },
        Commands::Exit | Commands::Quit => cmd::session::cmd_session_exit()?,
        Commands::Chat { action } => match action {
            ChatAction::List { since, src } => cmd::conversations_cmd::cmd_chat_list(since, src)?,
            ChatAction::Show { date } => cmd::conversations_cmd::cmd_chat_show(date)?,
            ChatAction::Raw { date, conv } => cmd::conversations_cmd::cmd_chat_raw(date, conv)?,
            ChatAction::Search { query, limit, src } => {
                cmd::conversations_cmd::cmd_chat_search(query, limit, src).await?
            }
        },
        Commands::Conversations { action } => match action {
            ConversationsAction::Pull => cmd::conversations_cmd::cmd_conversations_pull().await?,
            ConversationsAction::Cleanup => {
                cmd::conversations_cmd::cmd_conversations_cleanup().await?
            }
            ConversationsAction::Reindex {
                raw_only,
                spans_only,
                rollups_only,
            } => {
                cmd::conversations_cmd::cmd_conversations_reindex(
                    cmd::conversations_cmd::ReindexArgs {
                        raw_only,
                        spans_only,
                        rollups_only,
                    },
                )
                .await?
            }
            ConversationsAction::Doctor => {
                cmd::conversations_cmd::cmd_conversations_doctor().await?
            }
            ConversationsAction::Preflight => {
                cmd::conversations_cmd::cmd_conversations_preflight().await?
            }
            ConversationsAction::Migrate {
                run,
                resume,
                discard_staging,
            } => {
                cmd::conversations_cmd::cmd_conversations_migrate(run, resume, discard_staging)
                    .await?
            }
            ConversationsAction::Rollback => {
                cmd::conversations_cmd::cmd_conversations_rollback().await?
            }
            ConversationsAction::Compact {
                date,
                since,
                force,
                if_stale,
                max_days,
                extractive_only,
                debug_prompt,
                skip_rollups,
            } => {
                cmd::conversations_cmd::cmd_conversations_compact(
                    cmd::conversations_cmd::CompactArgs {
                        date,
                        since,
                        force,
                        if_stale,
                        max_days,
                        extractive_only,
                        debug_prompt,
                        skip_rollups,
                    },
                )
                .await?
            }
            ConversationsAction::Rollup {
                week,
                month,
                all_missing,
                force,
                if_stale,
                max_weeks,
                max_months,
            } => {
                cmd::conversations_cmd::cmd_conversations_rollup(
                    cmd::conversations_cmd::RollupArgs {
                        week,
                        month,
                        all_missing,
                        force,
                        if_stale,
                        max_weeks,
                        max_months,
                    },
                )
                .await?
            }
        },
        Commands::Deploy { action } => match action {
            DeployAction::Up {
                build,
                detach,
                file,
            } => cmd::deploy::cmd_deploy_up(file.as_deref(), build, detach)?,
            DeployAction::Down { volumes, file } => {
                cmd::deploy::cmd_deploy_down(file.as_deref(), volumes)?
            }
            DeployAction::Status { file } => cmd::deploy::cmd_deploy_status(file.as_deref())?,
            DeployAction::Logs {
                service,
                follow,
                file,
            } => cmd::deploy::cmd_deploy_logs(file.as_deref(), service.as_deref(), follow)?,
            DeployAction::Build { file } => cmd::deploy::cmd_deploy_build(file.as_deref())?,
        },
        Commands::Ask {
            question,
            src,
            since,
            until,
            k,
            model,
            min_score,
            json,
            no_escalate,
            debug_prompt,
            strict_citations,
            continue_flag,
            new_flag,
            show_session,
            no_summarize,
            summarize_model,
        } => {
            cmd::conversations_cmd::cmd_ask(cmd::conversations_cmd::AskArgs {
                question,
                src,
                since,
                until,
                k,
                model,
                min_score,
                json,
                no_escalate,
                debug_prompt,
                strict_citations,
                continue_flag,
                new_flag,
                show_session,
                no_summarize,
                summarize_model,
            })
            .await?
        }
        #[cfg(feature = "sources")]
        Commands::Source { cmd } => cmd::source_cmd::handle(cmd).await?,
    }

    Ok(())
}

// ── Unified search handler ────────────────────────────────────────────────

#[cfg(feature = "sources")]
async fn cmd_search_unified(
    query: String,
    source: Vec<String>,
    result_type: String,
    only_sources: bool,
    only_patterns: bool,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let want_patterns =
        !only_sources && (only_patterns || result_type == "patterns" || result_type == "all");
    let want_sources =
        !only_patterns && (only_sources || result_type == "sources" || result_type == "all");

    // SOURCES SIDE
    let mut source_hits: Vec<retrieve::UnifiedHit> = Vec::new();
    if want_sources {
        let cfg = store::config::load_config()?;
        let emb_cfg = store::embedding::EmbeddingConfig::from_config(&cfg);
        let index_path = dirs::home_dir()
            .context("no home dir")?
            .join(".mur")
            .join("index");
        let vector_store = store::vector::factory::get_vector_store(&cfg, &index_path).await?;
        let tantivy = sources::tantivy::TantivyIndex::open_or_create(
            &dirs::home_dir().context("no home dir")?.join(".mur"),
        )?;
        let source_weights: std::collections::HashMap<String, f32> = {
            let store = sources::instance::SourceInstanceStore::default_store()?;
            store
                .list()?
                .into_iter()
                .map(|i| (i.id, i.weight))
                .collect()
        };
        let filter = store::vector::SearchFilter {
            source_ids: if source.is_empty() {
                None
            } else {
                Some(source)
            },
            since: None,
        };
        source_hits = retrieve::retrieve_unified(
            &query,
            vector_store,
            &tantivy,
            &emb_cfg,
            &source_weights,
            &filter,
            0,
            limit,
            0.35,
        )
        .await?;
    }

    // PATTERNS SIDE — keyword + scoring fallback (full unification deferred to §8.1).
    let pattern_results: Vec<(String, f64)> = if want_patterns {
        existing_pattern_search_names(&query)
            .await
            .unwrap_or_default()
    } else {
        vec![]
    };

    if json {
        let j = serde_json::json!({
            "patterns": pattern_results.iter().map(|(name, score)| serde_json::json!({
                "name": name,
                "score": score,
            })).collect::<Vec<_>>(),
            "sources": source_hits.iter().map(|u| serde_json::json!({
                "chunk_id": u.hit.chunk_id,
                "source_id": u.hit.source_id,
                "external_id": u.hit.external_id,
                "score": u.hit.score,
                "heading_path": u.hit.heading_path,
                "updated_at": u.hit.updated_at.to_rfc3339(),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }

    if !pattern_results.is_empty() {
        println!("## Patterns ({})", pattern_results.len());
        for (name, score) in &pattern_results {
            println!("  [{:.3}] {}", score, name);
        }
    }
    if !source_hits.is_empty() {
        if !pattern_results.is_empty() {
            println!();
        }
        println!("## Notes ({})", source_hits.len());
        for u in &source_hits {
            let hp = if u.hit.heading_path.is_empty() {
                String::new()
            } else {
                format!(" § {}", u.hit.heading_path.join(" / "))
            };
            println!(
                "  [{:.3}] {} / {}{}",
                u.hit.score, u.hit.source_id, u.hit.external_id, hp
            );
        }
    }
    if pattern_results.is_empty() && source_hits.is_empty() {
        println!("(no hits)");
    }
    Ok(())
}

/// Thin adapter that reuses the existing pattern scoring pipeline and returns
/// `(pattern_name, score)` pairs. Falls back to an empty vec on error so the
/// source-retrieval section still renders.
///
/// NOTE: The existing `cmd::pattern::cmd_search` prints directly with emojis
/// and gate-skip messages, so we duplicate its scoring call here and format
/// results ourselves inside the unified handler. Full merge through a single
/// ranker is spec §8.1 future work.
#[cfg(feature = "sources")]
async fn existing_pattern_search_names(query: &str) -> anyhow::Result<Vec<(String, f64)>> {
    use retrieve::scoring::score_and_rank;
    use store::yaml::YamlStore;

    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;
    let scored = score_and_rank(query, patterns);
    let results = scored
        .into_iter()
        .map(|sp| (sp.pattern.name.clone(), sp.score))
        .collect();
    Ok(results)
}

#[cfg(not(feature = "sources"))]
async fn cmd_search_unified(
    query: String,
    _source: Vec<String>,
    _result_type: String,
    _only_sources: bool,
    _only_patterns: bool,
    _limit: usize,
    _json: bool,
) -> anyhow::Result<()> {
    // Feature-off: fall back to the existing pattern search (old behaviour).
    cmd::pattern::cmd_search(&query)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Helper: parse a full argv and return the `Commands::Ask` variant fields
    /// we care about. Panics on any other variant.
    fn parse_ask(argv: &[&str]) -> (bool, Option<String>) {
        let cli = Cli::try_parse_from(argv).expect("parse argv");
        match cli.command {
            Commands::Ask {
                no_summarize,
                summarize_model,
                ..
            } => (no_summarize, summarize_model),
            _ => panic!("expected Ask variant"),
        }
    }

    #[test]
    fn ask_parses_no_summarize_flag() {
        let (no_summarize, model) = parse_ask(&["mur", "ask", "q?", "--no-summarize"]);
        assert!(no_summarize);
        assert!(model.is_none());
    }

    #[test]
    fn ask_parses_summarize_model_flag() {
        let (no_summarize, model) =
            parse_ask(&["mur", "ask", "q?", "--summarize-model", "qwen3:4b"]);
        assert!(!no_summarize);
        assert_eq!(model.as_deref(), Some("qwen3:4b"));
    }

    #[test]
    fn ask_rejects_no_summarize_with_summarize_model() {
        // clap `conflicts_with` on --no-summarize guards this.
        let r = Cli::try_parse_from([
            "mur",
            "ask",
            "q?",
            "--no-summarize",
            "--summarize-model",
            "qwen3:4b",
        ]);
        assert!(r.is_err(), "clap must reject the conflict");
        let msg = r.err().unwrap().to_string();
        assert!(
            msg.contains("--summarize-model") || msg.contains("summarize-model"),
            "error should mention the conflicting flag; got: {msg}"
        );
    }
}
