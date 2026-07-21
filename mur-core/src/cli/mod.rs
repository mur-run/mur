//! CLI entry point — `Cli` parser + top-level `Commands` enum. Sub-action
//! enums live in sibling modules. Pure clap derive types — no logic here.

pub mod actions;
pub mod agent;
pub mod murmur;
pub mod notes;
pub mod skill;

pub use actions::*;
pub use agent::*;
pub use skill::IntentAction;
pub use skill::SkillAction;

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
    /// Search notes + sources (unified).
    #[command(hide = true)]
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
        /// Team ID for cloud sync (env: MUR_TEAM_ID)
        #[arg(long, env = "MUR_TEAM_ID")]
        team: Option<String>,
        #[command(subcommand)]
        action: Option<SyncAction>,
    },
    /// Inject patterns for a query (hook integration)
    #[command(hide = true)]
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
    #[command(hide = true)]
    Murmurd {
        #[command(subcommand)]
        action: MurmurdAction,
    },
    /// Run a workflow by name or semantic query
    #[command(hide = true)]
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
    /// Manage workflows
    Workflow {
        #[command(subcommand)]
        action: WorkflowAction,
    },
    /// Interact with a channel (HITL approval, etc.)
    Channel {
        #[command(subcommand)]
        action: ChannelAction,
    },
    /// Rebuild index from YAML files
    #[command(hide = true)]
    Reindex {
        /// Initialise the versioned git store and commit all existing patterns
        /// in one bootstrap commit. Run once to enable `mur pattern history`.
        #[arg(long)]
        bootstrap: bool,
    },
    /// Check for and install the latest mur release
    Update {
        /// Only check whether a newer version exists; don't install
        #[arg(long)]
        check: bool,
    },
    /// Show workflow composition & decomposition suggestions and pending nudges.
    #[command(hide = true)]
    Suggest {
        /// Auto-create suggested workflows/patterns as drafts
        #[arg(long)]
        create: bool,
        /// Accept a pending nudge by id -> create its draft workflow
        #[arg(long, value_name = "ID")]
        accept: Option<String>,
        /// Dismiss a pending nudge by id (never re-surfaces)
        #[arg(long, value_name = "ID")]
        dismiss: Option<String>,
    },
    /// Terminal dashboard
    Dashboard,
    /// Inject context-aware patterns (auto-detects project from pwd)
    #[command(hide = true)]
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
    /// Team shared skills
    Team {
        #[command(subcommand)]
        action: TeamAction,
    },
    /// Authentication (login / logout)
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Background daemon and server management
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
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
    #[command(hide = true)]
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
    /// Import/export patterns in MKEF (MUR Knowledge Exchange Format)
    #[command(hide = true)]
    Exchange {
        #[command(subcommand)]
        action: ExchangeAction,
    },
    /// Manage murmur agents (create, list, status, send, stop, ...)
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },
    /// Manage skills — list, install, author, audit, and evolve.
    #[command(after_help = "Command groups:
  Everyday:    list, show, search, info, install, remove, update, upgrade
  Authoring:   new, edit, validate, fmt, deps, publish, exchange, drafts
  Trust:       audit, trust, scope, curate
  Lifecycle:   stats, doctor, pin, unpin, sweep, archive, consolidate,
               evolve, generate, suggest, recombine, credit, eval

Internal/plumbing commands (schema, registry-index, reindex-stats,
reindex-vec, intent) are hidden from this list but still available;
see `mur skill <command> --help`.")]
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Manage notes — category:note skills with markdown bodies.
    Notes {
        #[command(subcommand)]
        action: notes::NotesAction,
    },
    /// Manage shared model registry (~/.mur/models.yaml)
    Model(cmd::model::ModelArgs),
    /// One-shot data migrations (workflow-engine v2)
    #[command(hide = true)]
    Migrate {
        /// Export ~/.mur/patterns to markdown, then delete patterns + fingerprints
        #[arg(long)]
        patterns: bool,
    },
    /// Verify documentation claims (paths, commands, code refs) against actual codebase
    #[command(hide = true)]
    Verify {
        /// Specific file to verify (default: scan all docs)
        #[arg(long)]
        file: Option<String>,
        /// Show all claims (including valid and skipped)
        #[arg(long)]
        all: bool,
    },
    /// Mark the current session important (ambient capture), or start recording +
    /// inject context (legacy manual mode) — depends on `session.capture`.
    #[command(hide = true)]
    In {
        /// Source identifier (e.g. claude-code)
        #[arg(long, default_value = "claude-code")]
        source: String,
    },
    /// Stop session recording with post-session menu (shorthand for session stop + next action)
    #[command(hide = true)]
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
    #[command(hide = true)]
    Drafts {
        #[command(subcommand)]
        action: DraftsAction,
    },
    /// Stop recording without export (alias: quit)
    #[command(hide = true)]
    Exit,
    /// Stop recording without export (alias: exit)
    #[command(hide = true)]
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
    #[command(hide = true)]
    Conversations {
        #[command(subcommand)]
        action: ConversationsAction,
    },
    /// Ask a natural-language question about your conversation archive (Mode C).
    #[command(hide = true)]
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
    /// Run an eval suite against the local pattern store
    #[command(hide = true)]
    Eval {
        #[command(subcommand)]
        action: EvalAction,
    },
    /// Configure the daemon sleep cycle (idle background learning).
    #[command(hide = true)]
    Sleep {
        #[command(subcommand)]
        action: SleepAction,
    },
    /// Index and search project source code
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Compress stdin or a file; print the result + token delta on stderr
    #[command(hide = true)]
    Compress {
        /// File to compress; omit (or pass `-`) to read stdin
        #[arg(long)]
        file: Option<String>,
        /// Optional semantic filter for scoring
        #[arg(long)]
        query: Option<String>,
    },
    /// Retrieve original content by blake3 hash
    #[command(hide = true)]
    Retrieve {
        /// Blake3 hash returned by `mur compress`
        hash: String,
        /// Optional filter query (returns matching subset)
        #[arg(long)]
        query: Option<String>,
    },
    /// Manage fleets — squads of agents working a shared goal
    Fleet {
        #[command(subcommand)]
        action: FleetAction,
    },
    /// Commander governance: pin the operator key + issue/inspect directives
    Commander {
        #[command(subcommand)]
        action: CommanderAction,
    },
    /// MUR-native deep research (wizard: `setup`; status: bare; run: pass a question)
    #[command(args_conflicts_with_subcommands = true)]
    DeepResearch {
        #[command(subcommand)]
        action: Option<DeepResearchAction>,
        /// Research question — runs the deep-research fleet directly
        question: Option<String>,
    },
    /// Official MUR catalog: browse and install official agents/fleets
    Official {
        #[command(subcommand)]
        action: OfficialAction,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_fleet_create() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "mur",
            "fleet",
            "create",
            "dev",
            "--members",
            "pm,qa",
            "--goal",
            "ship",
        ])
        .unwrap();
        match cli.command {
            Commands::Fleet {
                action:
                    crate::cli::actions::FleetAction::Create {
                        name,
                        members,
                        goal,
                        ..
                    },
            } => {
                assert_eq!(name, "dev");
                assert_eq!(members, vec!["pm".to_string(), "qa".to_string()]);
                assert_eq!(goal.as_deref(), Some("ship"));
            }
            _ => panic!("expected fleet create"),
        }
    }

    #[test]
    fn cli_parses_official_install() {
        use clap::Parser;
        let cli =
            Cli::try_parse_from(["mur", "official", "install", "fleets/deep-research"]).unwrap();
        match cli.command {
            Commands::Official {
                action: crate::cli::actions::OfficialAction::Install { id },
            } => assert_eq!(id, "fleets/deep-research"),
            _ => panic!("expected official install"),
        }
    }
}
