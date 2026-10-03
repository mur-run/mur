use super::*;

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
        /// Create a new channel and record execution on it. `delegate_to`
        /// steps only call their member on a channel run; each member is
        /// first checked for write access to this directory's git root.
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
    /// Delete a workflow from the server and from disk.
    ///
    /// Deleting only the local file does not stick: `mur sync` pulls every
    /// workflow the server holds and writes it back. This removes the server
    /// copy — for every device on the account — then the local file.
    Delete {
        /// Workflow name
        name: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
        /// Remove only the local file, leaving the server copy in place.
        /// It will return on the next sync; useful only for a workflow that
        /// was never published.
        #[arg(long = "local-only")]
        local_only: bool,
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
pub enum OpenAction {
    /// Record something an agent says is still outstanding
    Add {
        /// One line, imperative where possible
        title: String,
        /// Which agent is claiming it
        #[arg(long, default_value = "mur")]
        agent: String,
        /// The command or place that resolves it
        #[arg(long)]
        next: Option<String>,
    },
    /// Mark a reported item resolved (observed items clear themselves)
    Done {
        /// Item id shown by `mur open`, or the item's exact title
        id: String,
    },
    /// Stop showing a source. Exact `origin` match — `fleet` never matches
    /// `fleet:acme`.
    Mute {
        /// Origin as shown in brackets, e.g. `inbox` or `fleet:acme`
        origin: String,
    },
    /// Show a muted source again
    Unmute {
        /// Origin as shown in brackets, e.g. `inbox` or `fleet:acme`
        origin: String,
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
        /// Action to perform: analyze, export, skip, reject
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
