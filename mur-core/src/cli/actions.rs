//! Subcommand action enums (non-agent). Extracted from `main.rs` to keep the
//! binary entry point lean. Pure clap derive types — no logic lives here.

use crate::cmd::{browser::BrowserEngine, official::OfficialModelPolicy};
use clap::Subcommand;

mod browser;
mod chat;
mod fleet;
mod workflow;

pub use browser::*;
pub use chat::*;
pub use fleet::*;
pub use workflow::*;

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
    /// Restart the murmurd daemon (stop, wait for the old pid to exit, start
    /// detached) — the way to move a running daemon onto an upgraded binary
    Restart,
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
    /// Restart the murmurd daemon (stop, wait, start detached)
    Restart,
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
    /// Classify legacy channels missing a `purpose` (dry run unless --apply)
    BackfillPurpose {
        /// Write the inferred purposes to disk
        #[arg(long)]
        apply: bool,
        /// Maximum channels to classify and write; already-classified channels are skipped without counting toward it
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
}
