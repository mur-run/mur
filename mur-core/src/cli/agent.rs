//! `mur agent <subcommand>` clap definitions. Extracted from `main.rs`.

use clap::Subcommand;

use crate::cmd;

mod override_args;
pub use override_args::OverrideArgs;
mod config;
mod ops;
pub use config::{
    AgentAddonAction, AgentMcpAction, AgentPermAction, AgentPromptAction, AgentSkillAction,
};
pub use ops::{
    AgentEvalAction, AgentHooksAction, AgentPendingAction, AgentQueueAction, AgentScheduleAction,
    AgentSecretAction, AgentTrashAction, AgentTurnAction, AgentWebhookAction, SnapshotAction,
    VoiceAction,
};

#[derive(Subcommand)]
pub enum AgentAction {
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
    /// Start a stopped agent: via its launchd/systemd service when one is
    /// installed, else a detached (unsupervised) spawn of its runtime
    Start {
        /// Agent name
        name: String,
        #[command(flatten)]
        overrides: OverrideArgs,
    },
    /// Stop a running agent and keep it stopped: unloads its launchd/systemd
    /// service first when one is installed (otherwise KeepAlive respawns it a
    /// second later), then SIGTERM, then SIGKILL after the timeout. `start`
    /// loads the service again.
    Stop {
        /// Agent name
        name: String,
    },
    /// Gracefully restart one or more running agents so they pick up an
    /// upgraded runtime binary. Sends SIGTERM (drains in-flight turn), waits
    /// for exit, then polls for the launchd-respawned process.
    Restart {
        /// Agent name(s) (mutually exclusive with --all / --stale)
        #[arg(num_args = 0..)]
        names: Vec<String>,
        /// Restart all running agents
        #[arg(long)]
        all: bool,
        /// Restart only agents running a stale binary (build-id differs from on-disk)
        #[arg(long = "stale")]
        stale: bool,
        /// Print targets without restarting
        #[arg(long = "dry-run")]
        dry_run: bool,
        #[command(flatten)]
        overrides: OverrideArgs,
    },
    /// Remove an agent: deletes its runtime symlink and its launchd/systemd
    /// service, and leaves `~/.mur/agents/<name>/` in place unless --purge.
    /// Stop the agent first — removal refuses while it is running.
    Remove {
        /// Agent name
        name: String,
        /// Also delete the agent's data directory
        #[arg(long)]
        purge: bool,
        /// Skip the unread-inbox guard (R13)
        #[arg(long)]
        force: bool,
    },
    /// Rename an agent (updates dir, profile.name, and symlink)
    Rename {
        /// Old name
        old: String,
        /// New name
        new: String,
    },
    /// Set this agent's fallback chain of model_refs, tried in order after
    /// the primary model fails. Overrides the global `models.fallback_chain`
    /// (`mur model fallback`) when non-empty. Pass no refs to clear it.
    /// Each ref must already exist in `~/.mur/models.yaml`.
    Fallback {
        /// Agent name
        name: String,
        /// Ordered registry keys, tried after the primary fails. Omit to
        /// clear the chain.
        #[arg(num_args = 0..)]
        model_refs: Vec<String>,
    },
    /// Turn Smart background routing on or off for this agent, or `follow` to
    /// clear the override and inherit the global setting (`mur model smart`).
    Smart {
        /// Agent name
        name: String,
        /// `on`, `off`, or `follow`.
        #[arg(value_parser = ["on", "off", "follow"])]
        state: String,
    },
    /// Dial an agent's Unix socket and issue A2A `message/send`
    Send {
        /// Agent name
        name: String,
        /// A2A message JSON: {"role":"user","parts":[...]}
        message: String,
        /// When set, the agent writes its complete output to this path and
        /// the runtime returns a file artifact reference instead of inline
        /// content (issue #715 Part B).
        #[arg(long)]
        output_artifact_path: Option<String>,
        /// When a tool call needs approval, wait for a human to answer it
        /// (`mur agent hitl <agent> approve <id>`) instead of failing at once
        /// with `hitl_approval_required`. Optional max wait (`30s`, `10m`,
        /// `1h`; default 30m); the agent's deadline still wins if sooner.
        /// Nobody answering in time is the same `hitl_approval_required`.
        #[arg(long, value_name = "DURATION", num_args = 0..=1, default_missing_value = "")]
        wait_approval: Option<String>,
    },
    /// Dial an agent's Unix socket and print its A2A Agent Card
    Card {
        /// Agent name
        name: String,
    },
    /// Call any A2A method on a running agent and print the raw JSON result.
    ///
    /// The operator escape hatch: `send` and `card` cover two of the runtime's
    /// methods, and everything else was reachable only from inside murmur or
    /// not at all. Examples: `memory/reload` (re-read skills and memories after
    /// changing them on disk), `tasks/list`, `tasks/cancel`, `turn/steer`,
    /// `model/set`. Methods are passed through as typed — the agent's own
    /// handler decides what it will answer.
    Dial {
        /// Agent name
        name: String,
        /// A2A method, e.g. `memory/reload`
        method: String,
        /// JSON params object. Defaults to `{}`.
        params: Option<String>,
    },
    /// Set, show, or clear an agent's per-turn effort
    /// (low | medium | high | xhigh | max). Unset means the API default, high.
    Effort {
        /// Agent name
        name: String,
        /// Level to set; omit to print the current setting
        level: Option<String>,
        /// Unset the level, restoring the API default
        #[arg(long)]
        clear: bool,
    },
    /// What the router actually did, per turn — including the background turns
    /// that never render a decision anywhere else. `↓` marks a model Smart
    /// chose for you rather than one you configured.
    Routing {
        /// Agent name
        name: String,
        /// Rows to print (newest first). Default 20.
        #[arg(long)]
        limit: Option<usize>,
        /// Only turns where Smart picked the model.
        #[arg(long = "downgrades-only")]
        downgrades_only: bool,
    },
    /// Who can do what — the dispatch index, derived from what the sandbox
    /// actually enforces. With no filter, prints the whole roster.
    Who {
        /// Only agents that explicitly hold this binary (e.g. `cargo`), plus
        /// the fleets that could be delegated the work
        #[arg(long)]
        can: Option<String>,
        /// Only agents with a skill whose name contains this
        #[arg(long)]
        skill: Option<String>,
        /// Whose dispatch permissions to evaluate (default: the concierge)
        #[arg(long = "as")]
        as_agent: Option<String>,
    },
    /// Interactive streaming TUI chat with an agent (the agent must be running)
    Cli {
        /// Agent name(s) — more than one opens each chat in its own split pane
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        /// Resume the most recent saved conversation for this agent
        #[arg(long)]
        resume: bool,
        /// Auto-approve every tool call for this session. This is the default;
        /// the flag stays so existing scripts keep working.
        #[arg(long, conflicts_with = "ask")]
        auto: bool,
        /// Ask before every tool call this session instead (`/auto` toggles
        /// it back). The old default.
        #[arg(long)]
        ask: bool,
        /// Visual skin: ansi (default; `dark` is an alias) | light | mur | clay
        #[arg(long)]
        skin: Option<String>,
        /// Plain line-based output (no full-screen TUI) — for screen readers,
        /// piping, and CI logs. Auto-enabled when stdout is not a terminal.
        #[arg(long)]
        plain: bool,
        /// Stop accepting new turns once session's estimated cost (USD)
        /// reaches ceiling. Omit for no limit.
        #[arg(long = "budget-usd")]
        budget_usd: Option<f64>,
        /// Auto-approve read-only bash commands (cat/ls/grep/git status/…).
        /// This is the default; the flag stays so existing scripts keep
        /// working.
        #[arg(long = "auto-reads", conflicts_with = "no_auto_reads")]
        auto_reads: bool,
        /// Prompt for read-only commands too — the old default. Only
        /// observable under `--ask`, where it is the difference between a
        /// session that stops on `git status` and one that does not.
        #[arg(long = "no-auto-reads")]
        no_auto_reads: bool,
        /// Watch a fleet's shared channel in a status band: job progress at
        /// rest, expanding when a member is blocked. Names the fleet, not the
        /// channel — `--fleet develop` watches `fleet-develop`.
        #[arg(long)]
        fleet: Option<String>,
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
    /// Export an agent to a portable `.muragent` (default, signed v2) or a
    /// legacy `.murpkg`. Recipients open it with the MUR Hub app or run it
    /// headless via `mur-agent-runtime --load <file>.muragent`.
    Export {
        /// Agent name
        name: String,
        /// Output path (e.g. agent.muragent or agent.murpkg).
        /// Defaults to `<name>.muragent` (or `.murpkg`) in the current directory.
        #[arg(long, short = 'o')]
        out: Option<String>,
        /// Format: "muragent" (default — signed v2 portable) or "pkg" (legacy v1)
        #[arg(long, default_value = "muragent")]
        format: String,
    },
    /// Install an agent from a signed `.muragent` package
    Install {
        /// Path to the `.muragent` file
        path: String,
        /// Bind to an existing model registry entry immediately (non-interactive).
        /// The ref must already exist in ~/.mur/models.yaml.
        /// Example: --model ollama_llama3_2_3b
        #[arg(long)]
        model: Option<String>,
        /// Install as a NEW, distinct agent under this name — a fresh
        /// profile.id (uuid v7) and a freshly minted, persisted Ed25519
        /// identity (never the source's private key). Refuses if an agent
        /// already exists at this name.
        #[arg(long = "as", value_name = "NAME")]
        as_name: Option<String>,
    },
    /// Uninstall an agent (data preserved at <home>/data unless --purge)
    Uninstall {
        /// Agent name
        name: String,
        /// Also delete the agent's data directory
        #[arg(long)]
        purge: bool,
    },
    /// Inspect a `.muragent` package — print manifest + signature status
    Inspect {
        /// Path to the `.muragent` file
        path: String,
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
    /// Open a single-use pairing window: show a QR + URI to pair a phone (MUR
    /// mobile app) with an agent over LAN. The code expires shortly and is
    /// single-use — re-run to add another device.
    Pair {
        /// Agent name to pair with (defaults to the concierge "mur").
        #[arg(default_value = "mur")]
        name: String,
    },
    /// List phones paired with this Mac (fingerprint + pubkey).
    Devices,
    /// Revoke a paired phone by fingerprint prefix (or full pubkey).
    Unpair {
        /// Device fingerprint (see `mur agent devices`) or full pubkey.
        fingerprint: String,
    },
    /// Run prereq checks for export targets (no build, just diagnostics).
    /// With NAME, runs per-agent health checks instead: model_ref, MCP command
    /// resolution, and filesystem grants that do not exist and will therefore
    /// be dropped when the sandbox seals. For stale runtime BINARIES across
    /// every running agent, use `mur agent runtime-doctor`.
    /// Show or edit this agent's limits: block (alias of `mur limits <name>`)
    Limits {
        /// Agent name
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
    /// The effective outbound network policy: the agent's own layer and every
    /// MCP server in one table. The agent's `allow_hosts` does not govern its
    /// MCP servers; this is where that shows.
    Network {
        /// Agent name (case-insensitive).
        name: String,
    },
    Doctor {
        /// Agent name to run per-agent health checks for. Omit for the
        /// existing export-prereq checks.
        name: Option<String>,
        /// What format the doctor should validate prereqs for: "gui" / "bin" / "pkg" / "all"
        #[arg(long, default_value = "all")]
        format: String,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
    },
    /// Check running agents for stale runtime binaries (build-sha compare).
    /// Exits non-zero if any agent is stale. This is the FLEET-wide binary
    /// check; for one agent's health — model, MCP servers, and filesystem
    /// grants the kernel will drop — use `mur agent doctor <name>`.
    #[command(name = "runtime-doctor")]
    RuntimeDoctor {
        /// Emit JSON array instead of human text
        #[arg(long)]
        json: bool,
        /// Re-point launchers that resolve to a runtime no installer refreshes
        /// (a Homebrew keg copy). Changes which binary the next start runs, so
        /// it never happens without this flag.
        #[arg(long)]
        fix: bool,
    },
    /// Install missing curated program dependencies for this agent (consent-gated)
    #[command(name = "install-deps")]
    InstallDeps {
        /// Agent name
        name: String,
        /// Only install this one program (by name)
        #[arg(long)]
        program: Option<String>,
        /// Skip the per-item confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// See or forget the approval decisions an agent remembers (list/forget)
    Hitl {
        /// Agent name
        agent: String,
        #[command(subcommand)]
        action: super::agent_hitl::AgentHitlAction,
    },
    /// Manage an agent's keychain-backed secrets (set/list/delete)
    Secret {
        /// Agent name
        agent: String,
        #[command(subcommand)]
        action: AgentSecretAction,
    },
    /// Manage the C5 webhook receiver for an agent (enable/disable/show/secret).
    Webhook {
        /// Agent name
        agent: String,
        #[command(subcommand)]
        action: AgentWebhookAction,
    },
    /// Aggregate B0 eval JSONL into a markdown report (B0 M11.4).
    /// Reads a JSONL file produced by `scripts/eval/{agentdojo,harmbench}/run.py`,
    /// buckets by `(suite, attack_category)`, applies the spec
    /// thresholds, exits 0 if all gates pass / 1 otherwise.
    Eval {
        #[command(subcommand)]
        action: AgentEvalAction,
    },
    /// Manage voice I/O (TTS + STT) for an agent.
    Voice {
        /// Agent name.
        name: String,
        #[command(subcommand)]
        action: VoiceAction,
    },
    /// Per-turn undo: list promoted turns or put the project back to a
    /// turn's before-state (spec §4.1 step 4)
    Turn {
        #[command(subcommand)]
        action: AgentTurnAction,
    },
    /// Manage lifecycle cron schedule entries (C4)
    Schedule {
        #[command(subcommand)]
        action: AgentScheduleAction,
    },
    /// Show the active hook chain for an agent (A1)
    Hooks {
        #[command(subcommand)]
        action: AgentHooksAction,
    },
    /// Migrate all agents from mur-agent-gui (v0) to mur-hub (v1). Idempotent.
    MigrateToHub,
    /// List peer agents on this host
    Peers {
        /// Emit JSON instead of a human-readable table
        #[arg(long)]
        json: bool,
    },
    /// Pull high-fitness skills from peers (M7c — pull-side propagation).
    Propagate {
        /// Agent name (required outside an agent runtime context)
        name: String,
        /// Scan and report; install nothing.
        #[arg(long)]
        dry_run: bool,
        /// Override propagate.max_per_sweep for this run.
        #[arg(long)]
        max: Option<usize>,
        /// Override propagate.min_fitness.
        #[arg(long)]
        min_fitness: Option<f64>,
        /// Override propagate.min_samples.
        #[arg(long)]
        min_samples: Option<u64>,
        /// Emit JSON outcome.
        #[arg(long)]
        json: bool,
    },
    /// Show version history for an agent's profile (requires versioned store)
    History {
        /// Agent name
        name: String,
    },
    /// Roll back an agent profile to a prior version (creates a new commit)
    Rollback {
        /// Agent name
        name: String,
        /// Version number to restore
        #[arg(long)]
        to: u32,
    },
    /// Manage pattern snapshot for an agent.
    Snapshot {
        #[command(subcommand)]
        action: SnapshotAction,
    },
    /// Flush an offline agent's evidence outbox into the pattern store.
    Reconnect {
        /// Agent name
        name: String,
    },
    /// Create or update an agent from a declarative manifest YAML.
    Apply {
        /// Path to AgentManifest YAML file
        #[arg(short = 'f', long)]
        file: std::path::PathBuf,
    },
    /// List or act on pending ingested items (A1)
    Pending {
        /// Agent name
        name: String,
        #[command(subcommand)]
        action: Option<AgentPendingAction>,
    },
    /// Manage the action task queue (list/pause/cancel/retry)
    Queue {
        /// Agent name
        name: String,
        #[command(subcommand)]
        action: AgentQueueAction,
    },
    /// Manage agent trash (list/restore/empty/now)
    Trash {
        /// Agent name
        name: String,
        #[command(subcommand)]
        action: AgentTrashAction,
    },
    /// Manage imported add-ons (Claude plugin-groups).
    Addon {
        #[command(subcommand)]
        action: AgentAddonAction,
    },
    /// Build a specialized agent: role -> drafts -> human review -> create + start.
    Wizard {
        /// Role preset id from the catalog, or omit for interactive selection / custom.
        #[arg(long)]
        role: Option<String>,
        /// Path the agent may read/write (defaults to current dir).
        #[arg(long)]
        workspace: Option<String>,
        /// Non-interactive: accept generated drafts without the editor gate (still prints them).
        #[arg(long)]
        headless: bool,
        /// Skip all LLM stages; use catalog stubs/templates only.
        #[arg(long = "no-llm")]
        no_llm: bool,
        /// Model-ref to embed in the agent profile (e.g. claude_sonnet, claude_opus).
        #[arg(long = "model-ref", default_value = crate::agent_wizard::DEFAULT_MODEL_REF)]
        model_ref: String,
        /// Skip the eval stage even when an LLM is present.
        #[arg(long = "no-eval")]
        no_eval: bool,
    },
}

#[cfg(test)]
mod tests;
