use super::*;

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
    /// Report the fleet's most recent run, rendered exactly as `mur job
    /// status` would (spec §4) — a lookup keyed by the fleet's channel plus
    /// the shared renderer, not a separate status computation.
    Status {
        /// Fleet name
        name: String,
    },
    /// Run a fleet: one iteration, or `--loop` for a guarded loop
    Run {
        /// Fleet name
        name: String,
        /// Optional job text — runs one-shot (jumps ahead of queue)
        job: Option<String>,
        /// Loop until the router converges or a bound trips (deadline / stuck / cost_usd)
        #[arg(long = "loop")]
        loop_flag: bool,
        /// Ignored since 2.79 (kept for old scripts); bounds are `mur fleet limits`
        #[arg(long)]
        max_iterations: Option<u32>,
        /// Wall-clock deadline in loop mode, e.g. 30s/5m/2h — prefer `mur fleet limits <name> --deadline`
        #[arg(long)]
        deadline: Option<String>,
        /// Legacy spelling of `limits.cost_usd` — prefer `mur fleet limits <name> --cost-usd`
        #[arg(long)]
        budget_usd: Option<f64>,
        /// Force Tier-1 per-track git worktree isolation for this run (one-shot only,
        /// not supported with --loop). Equivalent to MUR_PARALLEL_EXEC=1 for this invocation.
        #[arg(long)]
        worktree: bool,
        /// Record this run under a caller-chosen id (a tool that dispatched it
        /// and will poll `mur_job_status`). Default: a fresh id.
        #[arg(long, value_name = "RUN_ID")]
        run_id: Option<String>,
        /// Directory the work is in (absolute). Members are routed to its git
        /// repo root, or to the directory itself outside a checkout, after
        /// each is checked for write access there. Default: this process's
        /// cwd — right at a shell, wrong when spawned, so the agent runtime
        /// always passes it.
        #[arg(long, value_name = "DIR")]
        cwd: Option<std::path::PathBuf>,
        /// `--cwd` was not named by the caller but taken from the calling
        /// agent's session directory. Marks the routing note as a guess.
        #[arg(long, requires = "cwd")]
        cwd_inferred: bool,
    },
    /// Update a fleet's loop/auto-run config (trigger, budget, iteration cap,
    /// deadline, done-when policy). Only the flags you pass are changed —
    /// everything else already set is preserved.
    SetLoop {
        /// Fleet name
        name: String,
        /// manual | interval:<dur> | cron:<5-field POSIX expr>
        #[arg(long)]
        trigger: Option<String>,
        /// Ignored since 2.79 (kept for old scripts); bounds are `mur fleet limits`
        #[arg(long)]
        max_iterations: Option<u32>,
        /// Wall-clock deadline, e.g. 30s/5m/2h/1d (relative) — prefer `mur fleet limits <name> --deadline`
        #[arg(long)]
        deadline: Option<String>,
        /// Legacy spelling of `limits.cost_usd` — prefer `mur fleet limits <name> --cost-usd`
        #[arg(long)]
        budget_usd: Option<f64>,
        /// Completion policy: marker:<TEXT> (own-line sentinel), queue-empty
        /// (stop when nothing is queued), or leave unset for router judgment
        #[arg(long)]
        done_when: Option<String>,
    },
    /// Show or edit this fleet's limits: block (alias of `mur limits <name>`)
    Limits {
        /// Fleet name
        name: String,
        /// Emit JSON instead of the table
        #[arg(long)]
        json: bool,
        /// Wall clock for the unit of work, e.g. 30m / 2h / 1h30m
        #[arg(long)]
        deadline: Option<String>,
        /// Minutes of no progress before stopping (unattended) or warning (attended); `off` disables
        #[arg(long)]
        stuck: Option<String>,
        /// Cost cap in USD — applies only to metered models
        #[arg(long)]
        cost_usd: Option<f64>,
        /// Remove a key so the scope inherits it (repeatable)
        #[arg(long = "unset")]
        unset: Vec<String>,
    },
    /// Show how often pre-dispatch triage was right, from the calibration ledger
    Triage {
        /// Days to look back
        #[arg(long, default_value_t = crate::cmd::fleet::triage_report::DEFAULT_DAYS)]
        days: u32,
        /// Emit JSON instead of the table
        #[arg(long)]
        json: bool,
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
        /// Scope to one dispatch's jobs, by run id (or a prefix of one —
        /// matches every `--loop` iteration of that run too). Without this,
        /// counts mix the run just dispatched with everything already queued
        /// (see `mur fleet status`'s doc comment / issue #1508).
        #[arg(long)]
        since: Option<String>,
    },
    /// Cancel a queued job so no run picks it up
    Cancel {
        /// Fleet name
        name: String,
        /// Job id, or a unique prefix of one (see `mur fleet jobs`)
        id: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
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
    /// Report on the fleet's declared external program dependencies
    Doctor {
        /// Fleet name
        name: String,
    },
    /// Install missing curated program dependencies for this fleet (consent-gated)
    #[command(name = "install-deps")]
    InstallDeps {
        /// Fleet name
        name: String,
        /// Only install this one program (by name)
        #[arg(long)]
        program: Option<String>,
        /// Skip the per-item confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum CapabilityAction {
    /// List available capabilities
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show a capability's contents
    Show { name: String },
    /// Install a capability onto an agent
    Install {
        name: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        yes: bool,
    },
    /// Remove a capability from an agent
    Remove {
        name: String,
        #[arg(long)]
        agent: String,
    },
}
