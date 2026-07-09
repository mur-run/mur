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
    /// Test injection pipeline: show what skills would be injected for a query
    Inject {
        /// Query to test injection against
        query: String,
    },
    /// Inject context-aware skills (auto-detects project/session context)
    Context {
        /// Quiet mode — only output injected skills
        #[arg(long, short)]
        quiet: bool,
        /// Compact output
        #[arg(long)]
        compact: bool,
        /// Override auto-detected query
        #[arg(long)]
        query: Option<String>,
        /// Write context to ~/.mur/context.md
        #[arg(long)]
        file: bool,
        /// Token budget (default: 2000)
        #[arg(long, default_value = "2000")]
        budget: usize,
        /// Source tool identifier
        #[arg(long, default_value = "cli")]
        source: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Scope filter (repeatable key=value)
        #[arg(long)]
        scope: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum AuthAction {
    /// Log in to mur.run
    Login,
    /// Log out and clear stored credentials
    Logout,
}

#[derive(Subcommand)]
pub enum DaemonAction {
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
    /// Start the local API server for the web dashboard
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
    /// Configure the daemon sleep cycle
    Sleep {
        #[command(subcommand)]
        action: SleepAction,
    },
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
pub enum InternalsAction {
    /// Rebuild the LanceDB vector index from YAML skill files
    Reindex {
        /// Initialise the versioned git store and commit all existing skills
        /// in one bootstrap commit.
        #[arg(long)]
        bootstrap: bool,
    },
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
    /// One-shot: import legacy cli-sessions into Channels.
    MigrateChannels,
    /// Unified schedule view (agent/workflow/fleet) as JSON — Panel data source
    #[command(hide = true)]
    ScheduleStatus {
        /// Filter to one agent's entries (globals always included)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Context recommendations working directory JSON — Panel data source
    #[command(hide = true)]
    Recommend {
        /// Working directory recommend
        #[arg(long)]
        cwd: String,
        /// Max items
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
}

#[derive(Subcommand)]
pub enum ChannelAction {
    /// Approve (or deny) a pending HITL gate on a channel (v3c)
    Approve {
        /// Channel ID
        channel_id: String,
        /// HITL request ID (from the HitlRequest event)
        hitl_id: String,
        /// Deny instead of approve
        #[arg(long)]
        deny: bool,
        /// Optional reason recorded in the HitlResponse
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum WorkflowAction {
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
        /// Auto-approve all `needs_approval` steps
        #[arg(long)]
        yes: bool,
        /// Record execution as events on an existing channel ID
        #[arg(long, value_name = "CHANNEL_ID")]
        channel: Option<String>,
        /// Create a new channel and record execution on it
        #[arg(long, conflicts_with = "channel")]
        channel_new: bool,
    },
    /// Show workflow composition suggestions and pending nudges
    Suggest {
        /// Auto-create suggested workflows as drafts
        #[arg(long)]
        create: bool,
        /// Accept a pending nudge by id
        #[arg(long, value_name = "ID")]
        accept: Option<String>,
        /// Dismiss a pending nudge by id
        #[arg(long, value_name = "ID")]
        dismiss: Option<String>,
    },
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
    /// Mark the current session important (ambient capture mode), or start
    /// recording + inject context (legacy manual mode). Behavior depends on
    /// `session.capture`: with ambient capture (default) recording is always on,
    /// so this flags the session so the harvest gate keeps it.
    In {
        /// Source identifier (e.g. claude-code)
        #[arg(long, default_value = "claude-code")]
        source: String,
    },
    /// Stop session recording with post-session menu
    Out {
        /// Action to perform: analyze, export, skip
        #[arg(long)]
        action: Option<String>,
        /// Force LLM analysis even for short sessions
        #[arg(long)]
        force: bool,
    },
    /// Stop recording and delete the session (no export)
    Discard,
    /// Remove session recording(s)
    Remove {
        /// Session ID or prefix
        id: Option<String>,

        /// Remove all session recordings
        #[arg(long, conflicts_with = "id")]
        all: bool,

        /// Skip confirmation prompt
        #[arg(short, long)]
        force: bool,

        /// Show what would be deleted without actually deleting
        #[arg(long, requires = "all")]
        dry_run: bool,
    },
    /// Remove recordings past retention and run harvest housekeeping
    #[command(hide = true)]
    Gc,
}

#[derive(Debug, Subcommand)]
pub enum FleetAction {
    /// Create a new fleet (squad of agents + a goal + a shared channel)
    Create {
        /// Fleet name (lowercase slug)
        name: String,
        /// Comma-separated member agent names
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
        /// Router agent (defaults to the concierge `mur`)
        #[arg(long)]
        router: Option<String>,
        /// One-line goal
        #[arg(long)]
        goal: Option<String>,
    },
    /// List all fleets
    List,
    /// Show a fleet's roster + goal
    Show {
        /// Fleet name
        name: String,
    },
    /// Run a fleet: one iteration, or `--loop` for a guarded loop
    Run {
        /// Fleet name
        name: String,
        /// Optional job text — runs one-shot (jumps ahead of queue)
        job: Option<String>,
        /// Loop until the router converges or a guard trips (cap/deadline/stuck)
        #[arg(long = "loop")]
        loop_flag: bool,
        /// Max iterations in loop mode (overrides fleet.yaml `loop.max_iterations`)
        #[arg(long)]
        max_iterations: Option<u32>,
        /// Wall-clock deadline in loop mode, e.g. 30s/5m/2h (overrides fleet.yaml)
        #[arg(long)]
        deadline: Option<String>,
        /// Projected USD budget for the loop (overrides fleet.yaml `loop.budget_usd`)
        #[arg(long)]
        budget_usd: Option<f64>,
        /// Force Tier-1 per-track git worktree isolation for this run (one-shot only,
        /// not supported with --loop). Equivalent to MUR_PARALLEL_EXEC=1 for this invocation.
        #[arg(long)]
        worktree: bool,
    },
    /// Update a fleet's loop/auto-run config (trigger, budget, iteration cap,
    /// deadline, done-when marker). Only the flags you pass are changed —
    /// everything else already set is preserved.
    SetLoop {
        /// Fleet name
        name: String,
        /// manual | interval:<dur> | cron:<5-field POSIX expr>
        #[arg(long)]
        trigger: Option<String>,
        /// Iteration cap for the guarded loop
        #[arg(long)]
        max_iterations: Option<u32>,
        /// Wall-clock deadline, e.g. 30s/5m/2h/1d (relative, NOT a calendar date)
        #[arg(long)]
        deadline: Option<String>,
        /// Projected USD budget ceiling for the loop; required > 0 for daemon auto-run
        #[arg(long)]
        budget_usd: Option<f64>,
        /// Convergence marker: `marker:<TEXT>` (own-line sentinel), or leave unset for router judgment
        #[arg(long)]
        done_when: Option<String>,
    },
    /// Queue a job for a fleet (async; drained by `run` or the daemon)
    Send {
        /// Fleet name
        name: String,
        /// The job text (becomes the goal for one run)
        job: String,
    },
    /// List a fleet's jobs and their status
    Jobs {
        /// Fleet name
        name: String,
        /// Include terminal (done/failed/canceled) jobs
        #[arg(long)]
        all: bool,
    },
    /// Stop a fleet (kill-switch): disable auto-run and halt a running loop
    Stop {
        /// Fleet name
        name: String,
    },
    /// Start a fleet: clear the kill-switch
    Start {
        /// Fleet name
        name: String,
    },
    /// Export a fleet definition + its fleet-scoped skills to a signed .fleet bundle
    Export {
        name: String,
        /// Also bundle the member agents (profile minus signing key + skills)
        #[arg(long)]
        with_members: bool,
        /// Output path (default: <name>.fleet)
        #[arg(short = 'o', long)]
        out: Option<std::path::PathBuf>,
    },
    /// Import a fleet from a .fleet bundle (verifies signature, scans skills, confirms)
    Import {
        /// Path to the .fleet bundle
        file: std::path::PathBuf,
        /// Overwrite an existing fleet/skill of the same name
        #[arg(long)]
        force: bool,
        /// Skip member-agent install even if the bundle includes them
        #[arg(long)]
        no_members: bool,
        /// Pre-approve the install confirmation (still verifies + scans)
        #[arg(long)]
        yes: bool,
    },
    /// Delete fleet + shared channel (member agents NOT deleted)
    Delete {
        /// Fleet name
        name: String,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Add agent(s) to a fleet (member + channel role)
    Add {
        /// Fleet name
        name: String,
        /// Agent name(s) to add
        #[arg(required = true)]
        agents: Vec<String>,
    },
    /// Remove agent(s) from a fleet (member + channel)
    Remove {
        /// Fleet name
        name: String,
        /// Agent name(s) to remove
        #[arg(required = true)]
        agents: Vec<String>,
    },
    /// Show per-unit scores across all parallel tracks
    Compare {
        /// Fleet name
        name: String,
        /// Filter output to a specific unit name or prefix
        #[arg(long)]
        unit: Option<String>,
    },
    /// Run the LLM judge across all parallel tracks (populated by `fleet run`)
    Judge {
        /// Fleet name
        name: String,
        /// Write judge_stats.json to the fleet dir (CAS hit rate + cost ratio)
        #[arg(long)]
        stats: bool,
    },
    /// Execute cherry-pick assembly from the best-scoring track units
    Cherry {
        /// Fleet name
        name: String,
        /// Apply the assembly without prompting
        #[arg(long)]
        auto: bool,
        /// Copy cherry-result into the live project tree (default: fleet's git root)
        #[arg(long)]
        promote: bool,
        /// Override destination for --promote
        #[arg(long)]
        target: Option<std::path::PathBuf>,
    },
    /// Preview how partition-mode splits the target file across tracks
    PartitionPlan {
        /// Fleet name
        name: String,
    },
    /// Deterministically merge partition-mode run results into one file
    Merge {
        /// Fleet name
        name: String,
        /// Copy merged file into the live project tree
        #[arg(long)]
        promote: bool,
        /// Override destination for --promote
        #[arg(long)]
        target: Option<std::path::PathBuf>,
    },
    /// N-way concurrent line merge from a parallel run (experimental; requires MUR_PARALLEL_CONCURRENT=1)
    MergeConcurrent {
        /// Fleet name
        name: String,
        /// Write concurrent_stats.json (Spike-1 overlap rate)
        #[arg(long)]
        stats: bool,
        /// Copy merged result into live project (refused if overlaps remain)
        #[arg(long)]
        promote: bool,
        /// Override destination for --promote
        #[arg(long)]
        target: Option<std::path::PathBuf>,
    },
}

#[derive(clap::Subcommand)]
pub enum CommanderAction {
    /// Pin the commander public key (multibase). Refuses overwrite without --force.
    Pin {
        pubkey: String,
        #[arg(long)]
        force: bool,
    },
    /// Show whether a commander key is pinned.
    Status,
    /// Issue a signed directive into a fleet channel (v1 local delivery).
    Directive {
        fleet: String,
        /// kill | resume | budget-ceiling
        kind: String,
        #[arg(long)]
        budget_usd: Option<f64>,
    },
}

#[derive(Subcommand)]
pub enum DeepResearchAction {
    /// Create restricted worker agents that each mount the
    /// `research-gateway` MCP server (no egress of their own — the
    /// per-server egress grant is a separate consent step).
    Provision {
        /// Number of worker agents to create (default: DEFAULT_WORKER_COUNT)
        #[arg(long)]
        count: Option<usize>,
        /// Agent name prefix; workers are named `<prefix>_1..N`
        /// (default: DEFAULT_WORKER_PREFIX)
        #[arg(long)]
        prefix: Option<String>,
        /// `models.yaml` registry alias each worker's `model_ref` is bound
        /// to (default: DEFAULT_WORKER_MODEL, currently `claude_haiku`).
        /// Without this, workers fall to the `ollama/llama3.2:3b` StubEcho
        /// default with no real reasoning.
        #[arg(long)]
        model: Option<String>,
        /// After provisioning, also grant each worker's `research-gateway`
        /// server `BroadAudited` egress (allow-ALL-except-deny-list, routed
        /// through the audited proxy). A separate, explicit-consent step —
        /// omit this flag and workers keep NO outbound egress. Prompts
        /// `[y/N]` per worker unless `--yes`.
        #[arg(long)]
        grant_egress: bool,
        /// Denied host (repeatable) for the `--grant-egress` grant; ignored
        /// otherwise.
        #[arg(long = "deny-host")]
        deny_hosts: Vec<String>,
        /// Skip the `--grant-egress` consent prompt. Use for scripted /
        /// non-interactive grants.
        #[arg(long)]
        yes: bool,
    },
    /// Run a deep-research fleet's guarded loop (thin wrapper over
    /// `mur fleet run --loop` — see `cmd/fleet/loop_run.rs`). This drives
    /// only the loop's guard rails (iteration cap / deadline / budget /
    /// kill-switch / marker convergence); it does NOT reimplement or
    /// bypass anything the plain fleet loop already does.
    Run {
        /// Fleet name (as created by `mur fleet create`, typically after
        /// `mur deep-research provision`)
        name: String,
        /// Max iterations (overrides fleet.yaml `loop.max_iterations`)
        #[arg(long)]
        max_iterations: Option<u32>,
        /// Wall-clock deadline, e.g. 30s/5m/2h (overrides fleet.yaml)
        #[arg(long)]
        deadline: Option<String>,
        /// Budget ceiling in USD (overrides fleet.yaml `loop.budget_usd`)
        #[arg(long)]
        budget_usd: Option<f64>,
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
pub enum SyncAction {
    /// Show sync status (outbox/inbox queue depths, last fetch time)
    Status,
    /// Sync agent profiles + skills across devices (Pro feature)
    Fleet {
        #[arg(value_parser = clap::builder::EnumValueParser::<FleetSyncDir>::new())]
        direction: Option<FleetSyncDir>,
        /// Override local version on conflict
        #[arg(long)]
        force_local: bool,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum FleetSyncDir {
    Pull,
    Push,
    Both,
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
    /// Ask a natural-language question about your conversation archive
    Ask {
        /// Question to ask
        question: Option<String>,
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
        #[arg(long = "continue", conflicts_with = "new_flag")]
        continue_flag: bool,
        #[arg(long = "new", conflicts_with = "continue_flag")]
        new_flag: bool,
        #[arg(long, conflicts_with_all = ["continue_flag", "new_flag"])]
        show_session: bool,
        #[arg(long, conflicts_with = "summarize_model")]
        no_summarize: bool,
        #[arg(long)]
        summarize_model: Option<String>,
    },
    /// Run polling ingesters (Cursor/Gemini/Aider)
    Pull,
    /// Apply retention cleanup
    Cleanup,
    /// Rebuild LanceDB from raw + summaries
    Reindex {
        #[arg(long, conflicts_with_all = ["spans_only", "rollups_only"])]
        raw_only: bool,
        #[arg(long, conflicts_with_all = ["raw_only", "rollups_only"])]
        spans_only: bool,
        #[arg(long, conflicts_with_all = ["raw_only", "spans_only"])]
        rollups_only: bool,
    },
    /// Run conversation archive health checks
    Doctor,
    /// Check migration preconditions
    Preflight,
    /// Migrate from commander paths
    Migrate {
        #[arg(long)]
        run: bool,
        #[arg(long, conflicts_with_all = &["run", "discard_staging"])]
        resume: bool,
        #[arg(long, conflicts_with_all = &["run", "resume"])]
        discard_staging: bool,
    },
    /// Roll back to commander's old paths
    Rollback,
    /// Generate hybrid summaries for completed days
    Compact {
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        if_stale: bool,
        #[arg(long)]
        max_days: Option<u32>,
        #[arg(long)]
        extractive_only: bool,
        #[arg(long)]
        debug_prompt: bool,
        #[arg(long)]
        skip_rollups: bool,
    },
    /// Generate weekly + monthly rollup summaries
    Rollup {
        #[arg(long)]
        week: Option<String>,
        #[arg(long, conflicts_with = "week")]
        month: Option<String>,
        #[arg(long, conflicts_with_all = ["week", "month"])]
        all_missing: bool,
        #[arg(long)]
        force: bool,
        #[arg(long)]
        if_stale: bool,
        #[arg(long)]
        max_weeks: Option<u32>,
        #[arg(long)]
        max_months: Option<u32>,
    },
    /// Aggregate LLM call telemetry into per-stage cost report
    CostReport {
        #[arg(long, default_value = "7d")]
        since: String,
        #[arg(long)]
        json: bool,
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
        /// Path to a docker-compose file (long form only; `-f` is --follow)
        #[arg(long)]
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
        /// Force full rebuild ignoring mtime cache
        #[arg(long)]
        rebuild: bool,
        /// Less output
        #[arg(long)]
        quiet: bool,
        /// Run indexing in background (default: auto-detect based on chunk count)
        #[arg(long, conflicts_with = "foreground")]
        background: bool,
        /// Force foreground execution even for large projects
        #[arg(long, conflicts_with = "background")]
        foreground: bool,
    },
    /// Internal: spawned by `project index --background`. Not shown in help.
    #[command(hide = true)]
    IndexWorker {
        project_name: String,
        project_path: String,
        #[arg(long)]
        rebuild: bool,
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
        /// Search across ALL indexed projects (default: only the current directory's project)
        #[arg(long)]
        all: bool,
    },
    /// Show indexing status for a project
    Status {
        #[arg(long)]
        path: Option<String>,
    },
    /// List all indexed projects
    List,
    /// Remove an indexed project
    Remove {
        /// Path to the project (defaults to current directory)
        path: Option<String>,
    },
}
