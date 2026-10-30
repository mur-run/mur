//! Runtime-operation subcommand enums for `mur agent` (turn, snapshot, voice, hooks, pending, queue, trash, schedule, eval, webhook, secret).

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum AgentTurnAction {
    /// Promoted turns that still have an undo snapshot, newest first
    List {
        /// Agent name
        name: String,
        /// Emit JSON instead of the table
        #[arg(long)]
        json: bool,
    },
    /// Restore every file the turn changed. Lists paths that changed after
    /// the promote and asks before overwriting them — undo is last-write-wins.
    Undo {
        /// Agent name
        name: String,
        /// Turn id (see `mur agent turn list`)
        turn: String,
        /// Only show what would be restored / removed / overwritten
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum SnapshotAction {
    /// Pull (or refresh) the pattern snapshot for this agent.
    Pull {
        /// Agent name
        name: String,
        /// Preview what would be snapshotted without writing to disk.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the current snapshot ref for this agent.
    Show {
        /// Agent name
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum VoiceAction {
    /// Enable voice I/O (TTS + STT) for an agent.
    Enable {
        /// Kokoro voice ID (default: af_heart).
        /// Valid: af_heart, af_bella, af_nicole, am_adam, am_michael.
        #[arg(long)]
        voice_id: Option<String>,
    },
    /// Disable voice I/O for an agent.
    Disable,
    /// Download voice model weights (~1.4 GB; SHA-256 verified).
    Download,
}

#[derive(Subcommand, Debug)]
pub enum AgentHooksAction {
    /// Print the active hook chain (table or JSON)
    Show {
        /// Agent name
        name: String,
        /// Emit machine-readable JSON array
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum AgentPendingAction {
    /// List pending items for this agent
    List,
    /// Execute an action on a pending item
    Act {
        /// Pending item UUID
        id: String,
        /// Action ID from file_actions (e.g. "summarize")
        action_id: String,
    },
}

#[derive(Subcommand)]
pub enum AgentQueueAction {
    /// List all tasks in the queue
    List,
    /// Pause a running task at next checkpoint
    Pause {
        /// Task ID (UUID)
        id: String,
    },
    /// Resume a paused task
    Resume {
        /// Task ID (UUID)
        id: String,
    },
    /// Cancel a queued or running task
    Cancel {
        /// Task ID (UUID)
        id: String,
    },
    /// Retry a failed task
    Retry {
        /// Task ID (UUID)
        id: String,
    },
}

#[derive(Subcommand)]
pub enum AgentTrashAction {
    /// List trash contents
    List,
    /// Restore a file from trash
    Restore {
        /// Trash entry UUID
        id: String,
    },
    /// Permanently empty all trash
    Empty,
    /// Immediately delete a specific trash item
    Now {
        /// Trash entry UUID
        id: String,
    },
}

#[derive(Subcommand)]
pub enum AgentScheduleAction {
    /// Append a cron entry to the agent's lifecycle.schedule
    Add {
        /// Agent name
        name: String,
        /// 5-field POSIX cron expression (e.g. "30 9 * * 1-5")
        #[arg(long)]
        cron: String,
        /// Message text injected as a user turn when the cron fires
        #[arg(long)]
        message: String,
        /// Send the message to a different agent instead of self (optional)
        #[arg(long)]
        sends_to: Option<String>,
        /// Fire once, then retire. Cron has no year field, so a dated
        /// expression like `0 10 1 9 *` otherwise repeats every September.
        #[arg(long)]
        once: bool,
    },
    /// List all schedule entries for an agent
    List {
        /// Agent name
        name: String,
    },
    /// Remove a schedule entry by index (0-based, see `list` for indices)
    Remove {
        /// Agent name
        name: String,
        /// Entry index to remove
        index: usize,
    },
    /// Show next N fire times for each schedule entry
    Next {
        /// Agent name
        name: String,
        /// How many upcoming fires to show per entry (default: 5)
        #[arg(long, default_value_t = 5)]
        count: usize,
    },
    /// Schedules this agent has asked for and not been granted
    Proposals { name: String },
    /// Grant a proposed schedule — it becomes a real entry and fires
    Accept { name: String, id: String },
    /// Refuse a proposed schedule
    Decline { name: String, id: String },
    /// Add an idle trigger that fires when the agent has been idle for N seconds
    IdleAdd {
        /// Agent name
        name: String,
        /// Idle threshold in seconds; trigger fires when agent has been idle this long
        #[arg(long)]
        after_secs: u64,
        /// Message injected as a user turn when the trigger fires
        #[arg(long)]
        message: String,
        /// Send the message to a different agent instead of self (optional)
        #[arg(long)]
        sends_to: Option<String>,
        /// Per-trigger refire cooldown in seconds (default: 600)
        #[arg(long, default_value_t = 600)]
        cooldown_secs: u64,
        /// Suppress firing during the agent's configured quiet-hours window (default: true)
        #[arg(long, default_value_t = true)]
        respect_quiet_hours: bool,
    },
    /// List all idle triggers for an agent
    IdleList {
        /// Agent name
        name: String,
    },
    /// Remove an idle trigger by index (0-based, see `idle-list` for indices)
    IdleRemove {
        /// Agent name
        name: String,
        /// Trigger index to remove
        index: usize,
    },
    /// Register the `skill-propagate` idle trigger with default settings.
    /// Idempotent — running twice does not duplicate the trigger.
    PropagateInit {
        /// Agent name
        name: String,
        /// Idle seconds before firing (default 1800 = 30 min)
        #[arg(long, default_value_t = 1800)]
        after_secs: u64,
        /// Cooldown between fires (default 7200 = 2 hr)
        #[arg(long, default_value_t = 7200)]
        cooldown_secs: u64,
    },
}

#[derive(Subcommand, Debug)]
pub enum AgentEvalAction {
    /// Aggregate JSONL → markdown report. Exits 1 on any spec-gate failure.
    Report {
        /// Path to the JSONL file to aggregate.
        #[arg(long = "jsonl")]
        jsonl: std::path::PathBuf,
        /// Optional markdown output path. Default: stdout.
        #[arg(long = "out")]
        out: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum AgentWebhookAction {
    /// Turn on the receiver. Writes `transport.webhook` into
    /// `profile.yaml`. The HMAC secret must be set first via
    /// `secret-set` — startup will refuse to bind otherwise.
    Enable {
        /// Override default bind address (`127.0.0.1`).
        #[arg(long)]
        bind: Option<String>,
        /// Override default port (`6789`).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Turn off the receiver. Leaves `hmac_secret_ref` in place so
    /// re-enabling doesn't need to re-set the secret.
    Disable,
    /// Print the current `transport.webhook` block in YAML.
    Show,
    /// Write the HMAC secret to the OS keychain
    /// (`service=mur-agent`, `account=<agent>/WEBHOOK_HMAC`). Reads
    /// from a hidden prompt when no inline VALUE is given.
    SecretSet {
        /// Optional value; omit to read from a hidden prompt.
        value: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum AgentSecretAction {
    /// Write a secret to the OS keychain (service=mur-agent,
    /// account=<agent>/<key>). Reads from stdin / hidden prompt if VALUE
    /// is omitted.
    Set {
        /// Secret key (e.g. ANTHROPIC_API_KEY).
        key: String,
        /// Optional value; omit to read from a hidden prompt.
        value: Option<String>,
    },
    /// List which secret refs the agent's active model expects, with status.
    List,
    /// Delete a keychain entry by key name. Idempotent.
    Delete {
        /// Secret key to delete.
        key: String,
    },
}
