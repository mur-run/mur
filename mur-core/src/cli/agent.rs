//! `mur agent <subcommand>` clap definitions. Extracted from `main.rs`.

use clap::Subcommand;

use crate::cmd;

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
    /// Interactive streaming TUI chat with an agent (the agent must be running)
    Cli {
        /// Agent name(s) — more than one opens each chat in its own split pane
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        /// Resume the most recent saved conversation for this agent
        #[arg(long)]
        resume: bool,
        /// Auto-approve every tool call for this session (no HITL prompts)
        #[arg(long)]
        auto: bool,
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
    /// legacy `.murpkg`. Recipients open it with the MuR Hub app or run it
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
    /// Run prereq checks for export targets (no build, just diagnostics)
    Doctor {
        /// What format the doctor should validate prereqs for: "gui" / "bin" / "pkg" / "all"
        #[arg(long, default_value = "all")]
        format: String,
        /// Emit JSON instead of human text
        #[arg(long)]
        json: bool,
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

#[derive(Subcommand)]
pub enum AgentPermAction {
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
    /// Set tool policy: allow | ask | deny
    ToolAllow { name: String, pattern: String },
    /// Set tool policy to ask (HITL prompt before execution)
    ToolAsk { name: String, pattern: String },
    /// Deny a tool pattern entirely
    ToolDeny { name: String, pattern: String },
    /// Remove a tool rule
    ToolClear { name: String, pattern: String },
    /// List all tool rules
    ToolList { name: String },
}

#[derive(Subcommand)]
pub enum AgentSkillAction {
    /// List attached skill ids
    List { name: String },
    /// Validate a skill and install it into the agent, then register it.
    /// The source is parsed (`.yaml`/`.yml`, or `.md` which is accepted and
    /// converted to canonical YAML), schema-validated and security-scanned,
    /// then written into a per-skill subdir as `skills/<name>/skill.yaml`
    /// (name taken from the manifest). Sources that aren't valid skill
    /// manifests are rejected rather than stored as dead files.
    Add {
        name: String,
        /// Path to a skill source (`.yaml`/`.yml` or `.md`). Stored id is
        /// `skills/<manifest-name>`.
        source: String,
    },
    /// Remove a skill entry; deletes the backing subdir if orphaned.
    /// `skill_id` may be the full id (`skills/foo`), the basename (`foo`),
    /// or the basename without extension.
    Remove { name: String, skill_id: String },
    /// Print the canonical contents of an installed skill.
    /// `skill_id` may be the full id (`skills/foo`), the basename (`foo`),
    /// or the basename without extension.
    Show { name: String, skill_id: String },
}

#[derive(Subcommand)]
pub enum AgentMcpAction {
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
        /// Skip the y/N install confirmation prompt (B0 rule 6 / M9.2).
        /// Use for scripted / non-interactive installs.
        #[arg(long)]
        force: bool,
        /// Publisher name shown in the install prompt and pinned into
        /// the profile for audit (e.g. "Anthropic" or "@alice").
        #[arg(long = "publisher-name")]
        publisher_name: Option<String>,
        /// Publisher homepage URL (display-only).
        #[arg(long = "publisher-homepage")]
        publisher_homepage: Option<String>,
        /// Publisher registry coordinate (display-only,
        /// e.g. "@anthropic-mcp/weather@1.2.3").
        #[arg(long = "publisher-registry-id")]
        publisher_registry_id: Option<String>,
    },
    /// Remove an MCP server entry by id
    Remove { name: String, server_id: String },
    /// Rename an MCP server entry in place
    Rename {
        name: String,
        old: String,
        new: String,
    },
    /// Show pinned vs current state for one (or all) MCP entries.
    /// Exit codes (B0 rule 6 / M9.4): 0 clean, 1 binary drift, 4
    /// unpinned (pre-M9), 5 binary missing. Reports the worst status
    /// across all servers when `--server` is omitted.
    Inspect {
        name: String,
        /// Limit to a single MCP server entry; otherwise iterate all.
        #[arg(long = "server")]
        server: Option<String>,
        /// Spawn the MCP and verify `tools/list` against the pinned
        /// description hash (B0 rule 6 / M9.3.5). Off by default —
        /// inspect stays fast; pass `--probe` for the active scan.
        #[arg(long)]
        probe: bool,
    },
    /// Re-compute and persist the install-time binary pin (and
    /// optionally refresh publisher metadata). Used after reviewing
    /// drift surfaced by `mur agent mcp inspect`.
    Pin {
        name: String,
        server_id: String,
        /// Skip the y/N re-approval prompt.
        #[arg(long)]
        force: bool,
        /// Skip the live MCP probe (M9.3.5). Use for MCPs that can't
        /// be cleanly probed (slow boot, side-effecting init, etc.).
        /// `description_hash` stays at its previous value.
        #[arg(long = "no-probe")]
        no_probe: bool,
        #[arg(long = "publisher-name")]
        publisher_name: Option<String>,
        #[arg(long = "publisher-homepage")]
        publisher_homepage: Option<String>,
        #[arg(long = "publisher-registry-id")]
        publisher_registry_id: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum AgentPromptAction {
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

#[cfg(test)]
mod tests {
    use super::AgentAction;
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    fn parse_cli_action(argv: &[&str]) -> AgentAction {
        let cli = Cli::try_parse_from(argv).expect("parse argv");
        match cli.command {
            Commands::Agent { action } => action,
            _ => panic!("expected Agent variant"),
        }
    }

    #[test]
    fn agent_cli_accepts_multiple_names() {
        let AgentAction::Cli {
            names,
            resume,
            auto,
        } = parse_cli_action(&["mur", "agent", "cli", "a1", "a2", "a3", "--auto"])
        else {
            panic!("expected Cli variant");
        };
        assert_eq!(names, vec!["a1", "a2", "a3"]);
        assert!(!resume);
        assert!(auto);
    }

    #[test]
    fn agent_cli_single_name_still_parses() {
        let AgentAction::Cli { names, .. } = parse_cli_action(&["mur", "agent", "cli", "mur"])
        else {
            panic!("expected Cli variant");
        };
        assert_eq!(names, vec!["mur"]);
    }

    #[test]
    fn agent_cli_requires_at_least_one_name() {
        assert!(Cli::try_parse_from(["mur", "agent", "cli"]).is_err());
    }
}
