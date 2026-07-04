//! `mur skill` subcommand surface.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum SkillAction {
    /// Scaffold a new skill manifest from a template.
    New {
        /// Skill name (lowercase-kebab, e.g. `my-skill`).
        name: String,
        /// Skill category: context, workflow, command, meta, or note.
        #[arg(long, default_value = "context")]
        category: String,
        /// Write the scaffold under this directory instead of the current dir.
        #[arg(long)]
        dir: Option<String>,
        /// Install into the agent's loadable layout (<MUR_HOME>/agents/<agent>/skills/).
        #[arg(long)]
        agent: Option<String>,
        /// Overwrite an existing skill.yaml.
        #[arg(long)]
        force: bool,
    },
    /// Open a skill's skill.yaml in $EDITOR, then validate it.
    Edit {
        /// Skill name.
        name: String,
        /// Resolve under the agent's loadable layout.
        #[arg(long)]
        agent: Option<String>,
        /// Resolve under this directory instead of the current dir.
        #[arg(long)]
        dir: Option<String>,
    },
    /// Run schema validation + full security content scan on a skill file.
    Validate {
        #[arg(default_value = "skill.yaml")]
        path: String,
        #[arg(long)]
        warnings_only: bool,
    },
    /// Emit the JSON Schema of the skill manifest (for the Hub DAG editor).
    Schema {
        /// Write to a file instead of stdout
        #[arg(long)]
        out: Option<String>,
    },
    /// Convert between canonical YAML and markdown frontmatter.
    Fmt {
        path: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        write: bool,
    },
    /// List installed skills (from ~/.mur/skills/).
    List,
    /// Show full content of an installed skill.
    Show { name: String },
    /// Uninstall a skill.
    Remove { name: String },
    /// Set a skill's visibility scope (so the scope-aware injector only surfaces
    /// it in the matching context). Choose exactly one of the flags.
    Scope {
        /// Skill name
        name: String,
        /// Scope to a fleet (by name)
        #[arg(long)]
        fleet: Option<String>,
        /// Scope to the current git repo
        #[arg(long)]
        project: bool,
        /// Scope to a MUR Server team (by id)
        #[arg(long)]
        team: Option<String>,
        /// Reset to user scope (global)
        #[arg(long)]
        user: bool,
    },
    /// Search installed skills (--local) or remote registry.
    Search {
        query: String,
        #[arg(long)]
        local: bool,
    },
    /// Show Layer 1+2 summary of an installed skill.
    Info {
        name: String,
        #[arg(long)]
        full: bool,
        /// Show runtime metrics (usage, confidence, lifecycle state).
        #[arg(long)]
        metrics: bool,
    },
    /// Run full security scan + signature check on an installed skill.
    Audit { name: String },
    /// Promote or demote a skill's trust level.
    Trust {
        name: String,
        #[arg(long)]
        level: String,
    },
    /// Install a skill from registry, file, or URL.
    Install {
        /// Skill name (registry), local path, or git URL.
        source: String,
    },
    /// Publish a local skill to the default registry.
    Publish {
        /// Path to skill.yaml to publish.
        path: String,
    },
    /// Validate all skills in a registry checkout and (re)generate its
    /// `index.yaml`. With `--check`, verify the on-disk index is authoritative
    /// (CI gate) instead of writing. Validates signatures + security scan;
    /// never signs.
    RegistryIndex {
        /// Path to the skill-registry checkout (contains `skills/` + `index.yaml`).
        dir: String,
        /// Verify the on-disk index is authoritative (exit non-zero on mismatch).
        #[arg(long)]
        check: bool,
    },
    /// Update an installed skill to the latest registry version.
    Update {
        /// Name of installed skill to update.
        name: String,
    },
    /// Upgrade all origin-stamped (registry-installed) skills to the latest
    /// registry version. Skips anything user-modified since install.
    Upgrade {
        /// Report what would be upgraded without writing anything.
        #[arg(long)]
        check: bool,
        /// Emit the report as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Print the resolved dependency tree for an installed skill.
    Deps {
        /// Name of installed skill.
        name: String,
    },
    /// Generate a skill from a session recording.
    Generate {
        #[arg(long)]
        from_session: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "4")]
        parallel: usize,
    },
    /// Scan recent sessions for repeat task patterns (>=3 occurrences).
    Suggest {
        /// Max sessions to scan (default 20).
        #[clap(long, default_value = "20")]
        max_sessions: usize,
        /// Min session count to flag a pattern (default 3).
        #[clap(long, default_value = "3")]
        threshold: usize,
    },
    /// Self-evolve a skill by analyzing telemetry and applying minimal fixes.
    Evolve {
        /// Skill name to evolve.
        name: String,
        /// Preview changes without writing.
        #[clap(long)]
        dry_run: bool,
        /// Max evolution iterations (default 3).
        #[clap(long, default_value = "3")]
        max_iterations: usize,
    },
    /// Show runtime statistics for an installed skill (M5a).
    Stats {
        /// Skill name.
        name: String,
        /// Aggregate stats across all peer agents.
        #[arg(long)]
        all_agents: bool,
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Pin a skill to prevent auto-demotion (M5a).
    Pin {
        /// Skill name.
        name: String,
        /// Reason for pinning.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Unpin a previously pinned skill (M5a).
    Unpin {
        /// Skill name.
        name: String,
    },
    /// Rebuild stats sidecars from the JSONL trace log (M5a).
    ReindexStats {
        /// Skill filter (exact name or glob, e.g. 'research-*').
        name: Option<String>,
        /// Days of traces to scan (default 30).
        #[arg(long, default_value = "30")]
        days_back: u32,
    },
    /// Run health checks on installed skills (M5a read-only).
    Doctor {
        /// Skill name(s) or glob pattern(s) to check (default: all installed).
        names: Vec<String>,
        /// Only run specific checks (tools,deps,recency,failure-rate,api-drift,coverage-gap,mcp-requirements-coverage,mcp-capability-available,intent-resolvable).
        #[arg(long, value_delimiter = ',')]
        check: Vec<String>,
        /// JSON output.
        #[arg(long)]
        json: bool,
        /// CI-friendly mode where warnings exit 1.
        #[arg(long)]
        strict: bool,
        /// Accepted for forward CLI stability; no-op in M5a.
        #[arg(long)]
        fix: bool,
        /// Accepted for forward CLI stability; no-op in M5a.
        #[arg(long)]
        apply: bool,
        /// Enable LLM-augmented checks (api-drift, coverage-gap).
        #[arg(long)]
        llm: bool,
        /// Show LLM maintenance status (model, budget, cache).
        #[arg(long)]
        llm_status: bool,
    },
    /// Run lifecycle sweep across installed skills (M5b).
    Sweep {
        /// Skill filter (exact name or glob, e.g. 'research-*').
        name: Option<String>,
        /// Preview transitions without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Archive a skill (M5b).
    Archive {
        /// Skill name.
        name: String,
        /// Reason for archival.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Rebuild skill embedding index (M6c.1).
    ReindexVec {
        /// Optional skill name to reindex; all if omitted.
        name: Option<String>,
        /// Remove embeddings for skills no longer installed.
        #[arg(long)]
        prune: bool,
    },
    /// Run consolidation pass: dedup + contradiction + orphan (M5b).
    Consolidate {
        /// Preview findings without writing.
        #[arg(long)]
        dry_run: bool,
        /// Apply changes (archive orphans, deprecate duplicates).
        #[arg(long)]
        apply: bool,
        /// Dedup method: jaccard (default), vector, or both.
        #[arg(long, value_enum, default_value_t = Method::Jaccard)]
        method: Method,
        /// Use LLM to adjudicate contradiction pairs.
        #[arg(long)]
        llm_adjudicate: bool,
        /// Run cross-agent duplicate scan across peer agents (M7a).
        #[arg(long)]
        cross_agent: bool,
    },
    /// Record a human curation event for an LLM-extracted skill, so it can
    /// promote past Emerging (amendment A1).
    Curate {
        /// Skill name to curate.
        name: String,
    },
    /// Recombine two skills into a new Draft offspring on this agent.
    Recombine {
        /// First parent ref: `<name>` (local) or `agent://<peer>/<name>`.
        a: String,
        /// Second parent ref: `<name>` (local) or `agent://<peer>/<name>`.
        b: String,
        /// Combination strategy.
        #[arg(long, value_enum, default_value_t = RecombineStrategyArg::Union)]
        strategy: RecombineStrategyArg,
        /// Output skill name. Default: `<a>-x-<b>`.
        #[arg(long)]
        name: Option<String>,
        /// Print recombined manifest YAML to stdout without writing.
        #[arg(long)]
        dry_run: bool,
        /// Invoking agent (default: current agent from runtime context).
        #[arg(long)]
        agent: Option<String>,
        /// Emit JSON outcome record instead of human text.
        #[arg(long)]
        json: bool,
    },
    /// Show the credit lineage for a skill (M7c).
    Credit {
        /// Skill name
        name: String,
        /// Invoking agent (defaults to current)
        #[arg(long)]
        agent: Option<String>,
        /// Emit JSON
        #[arg(long)]
        json: bool,
    },
    /// Import/export skills in MKEF format
    Exchange {
        #[command(subcommand)]
        action: crate::cli::actions::ExchangeAction,
    },
    /// Manage pending skill drafts
    Drafts {
        #[command(subcommand)]
        action: crate::cli::actions::DraftsAction,
    },
    /// Run eval suites
    Eval {
        #[command(subcommand)]
        action: crate::cli::actions::EvalAction,
    },
    /// Host-level intent canonicalisation (M7c).
    #[command(subcommand)]
    Intent(IntentAction),
}

#[derive(Subcommand)]
pub enum IntentAction {
    /// Generate or update the host-level canonical intent mapping.
    Canonicalise {
        /// Preview the canonical mapping without writing.
        #[arg(long)]
        dry_run: bool,
        /// Emit JSON instead of YAML.
        #[arg(long)]
        json: bool,
    },
    /// Show the current canonical intent mapping.
    Show {
        /// Emit JSON instead of YAML.
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum Method {
    Jaccard,
    Vector,
    Both,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum RecombineStrategyArg {
    Union,
    Intersection,
    Llm,
}
