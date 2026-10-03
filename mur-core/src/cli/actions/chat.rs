use super::*;

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
        /// Index the main repository containing the current directory, following
        /// a linked worktree back to its primary repo. Used by the post-commit
        /// hook so the hook carries no hardcoded path.
        #[arg(long, conflicts_with = "path")]
        main_repo: bool,
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
        /// Machine-readable JSON (schema_version 1) instead of the text summary
        #[arg(long)]
        json: bool,
    },
    /// List all indexed projects
    List,
    /// Remove an indexed project
    Remove {
        /// Path to the project (defaults to current directory)
        path: Option<String>,
    },
}

/// Official MUR catalog actions.
#[derive(Subcommand, Debug)]
pub enum OfficialAction {
    /// List official agents and fleets from app.mur.run
    List,
    /// Download, verify, and install an official item (requires `mur auth login`)
    Install {
        /// Catalog id, e.g. agents/researcher or fleets/deep-research
        id: String,
        /// Automatically select a compatible model chain by policy
        #[arg(long, value_enum, conflicts_with = "model_ref")]
        model_policy: Option<OfficialModelPolicy>,
        /// Use this registered model as the primary
        #[arg(long)]
        model_ref: Option<String>,
        /// Ordered fallback model ref; may be repeated
        #[arg(long, requires = "model_ref")]
        fallback: Vec<String>,
    },
}
