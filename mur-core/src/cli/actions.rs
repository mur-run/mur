//! Subcommand action enums (non-agent). Extracted from `main.rs` to keep the
//! binary entry point lean. Pure clap derive types — no logic lives here.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum HookEvent {
    /// Handle UserPromptSubmit / BeforeAgent / beforeSubmitPrompt events
    Prompt {
        /// AI tool identifier (claude, gemini, cursor, copilot, opencode, amp)
        #[arg(long, default_value = "claude")]
        tool: String,
    },
    /// Handle PreToolUse / AfterTool / beforeShellExecution events
    Tool {
        /// AI tool identifier
        #[arg(long, default_value = "claude")]
        tool: String,
    },
    /// Handle Stop / SessionEnd events (triggers background pipeline)
    Stop {
        /// AI tool identifier
        #[arg(long, default_value = "claude")]
        tool: String,
    },
    /// Handle SessionStart events (injects L0 capability index in M2)
    #[command(name = "session-start")]
    SessionStart {
        /// AI tool identifier
        #[arg(long, default_value = "claude")]
        tool: String,
    },
    /// Show hook statistics (skip rate, tier distribution, latency)
    Stats,
}

#[derive(Subcommand)]
pub enum MurmurdAction {
    /// Start the murmurd daemon
    Start {
        /// Run in background (detach from terminal)
        #[arg(long)]
        detach: bool,
    },
    /// Stop the murmurd daemon
    Stop,
    /// Show murmurd daemon status
    Status,
}

#[derive(Subcommand)]
pub enum ExchangeAction {
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
pub enum LearnAction {
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
pub enum FeedbackAction {
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
pub enum PatternAction {
    /// Show a pattern by name (with attachments)
    Show { name: String },
    /// Show version history for a pattern
    History {
        /// Pattern name
        name: String,
    },
    /// Show diff between two pattern versions (defaults: current vs previous)
    Diff {
        /// Pattern name
        name: String,
        /// First version number (default: previous)
        v1: Option<u32>,
        /// Second version number (default: current)
        v2: Option<u32>,
    },
    /// Roll back a pattern to a prior version (creates a new commit)
    Rollback {
        /// Pattern name
        name: String,
        /// Version number to restore
        #[arg(long)]
        to: u32,
    },
}

#[derive(Subcommand)]
pub enum InternalsAction {
    /// Rebuild the versioned-store history index from git log (recovery only)
    RebuildIndex {
        /// Which layer: `knowledge` (patterns/workflows) or `agents`
        #[arg(long, default_value = "knowledge")]
        layer: String,
    },
    /// Run a raw git subcommand against the knowledge or agents repo
    Git {
        /// Which layer: `knowledge` or `agents`
        #[arg(long, default_value = "knowledge")]
        layer: String,
        /// Git arguments (e.g. `log --oneline -10`)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum GepAction {
    /// Run one GEP evolution generation
    Evolve,
    /// Show population fitness statistics
    Status,
}

#[derive(Subcommand)]
pub enum EvolveAction {
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
pub enum WorkflowAction {
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
pub enum ScheduleAction {
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
pub enum SessionAction {
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
        /// Run Reflector+Curator: update pattern confidence from session transcript.
        #[arg(long)]
        reflect: bool,
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
    /// Reflect + curate the last session transcript (E2).
    ///
    /// Runs the Reflector+Curator pipeline on the most recent recording:
    /// updates pattern confidence and evidence signals based on usage signals.
    Reflect {
        /// Preview changes without saving to the pattern store.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum CommunityAction {
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
pub enum TeamAction {
    /// List your teams (or patterns in a specific team)
    List {
        /// Team ID or slug (optional — lists your teams if omitted)
        #[arg(long, env = "MUR_TEAM_ID")]
        team: Option<String>,
    },
    /// Set the default team (saves to config so --team can be omitted)
    Use {
        /// Team slug or UUID
        team: String,
    },
    /// Share a pattern to your team
    Share {
        /// Pattern name
        name: String,
        /// Team ID or slug (falls back to default set by `mur team use`)
        #[arg(long, env = "MUR_TEAM_ID")]
        team: Option<String>,
    },
    /// Pull latest team patterns
    Sync {
        /// Team ID or slug (falls back to default set by `mur team use`)
        #[arg(long, env = "MUR_TEAM_ID")]
        team: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum PackAction {
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
pub enum SyncAction {
    /// Show sync status (outbox/inbox queue depths, last fetch time)
    Status,
}

#[derive(Subcommand)]
pub enum ChatAction {
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
pub enum ConversationsAction {
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
    /// Aggregate LLM call telemetry into per-stage cost report.
    CostReport {
        /// Time range relative to now (e.g. `7d`, `30d`, `1h`) or RFC3339 timestamp.
        #[arg(long, default_value = "7d")]
        since: String,
        /// Emit JSON instead of pretty table.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum DraftsAction {
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
pub enum DeployAction {
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

#[derive(Subcommand)]
pub enum EvalAction {
    /// Run a named eval suite
    Run {
        /// Suite name: retrieval | maturity | reflector | federation
        suite: String,
        /// Output format: text (default) | json
        #[arg(long, default_value = "text")]
        format: String,
    },
}

#[derive(Subcommand)]
pub enum SleepAction {
    /// Enable the daemon sleep cycle (idle background learning).
    Enable,
    /// Disable the daemon sleep cycle.
    Disable,
    /// Show current sleep cycle configuration.
    Status,
}

#[derive(Subcommand)]
pub enum ProjectAction {
    /// Index a project's source code for semantic search
    Index {
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        quiet: bool,
    },
    /// Search indexed code for a query
    Search {
        query: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "5")]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Show indexing status for a project
    Status {
        #[arg(long)]
        path: Option<String>,
    },
    /// List all indexed projects
    List,
}
