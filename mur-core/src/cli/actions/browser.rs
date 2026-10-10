use super::*;

#[derive(Subcommand)]
pub enum BrowserAction {
    /// Start an MCP proxy that records browser actions into a named run.
    Record {
        #[arg(long)]
        run: String,
        #[arg(long)]
        profile: Option<String>,
        /// `test` | `automation` | `live` (interactive, behind the egress proxy).
        #[arg(long, default_value = "test")]
        mode: String,
        #[arg(long)]
        trace: bool,
        /// Extra arguments passed verbatim to @playwright/mcp.
        #[arg(last = true, allow_hyphen_values = true)]
        extra: Vec<String>,
    },
    /// Replay a recorded browser run headlessly.
    Replay {
        name: String,
        /// Site profile to replay under (defaults to the one recorded with the run).
        #[arg(long)]
        profile: Option<String>,
        /// Self-heal steps whose locators all miss; verified heals are written back.
        #[arg(long)]
        heal: bool,
        /// Share of element steps allowed to heal in `mode: test` (0.0–1.0).
        #[arg(
            long,
            default_value_t = mur_browser::heal::DEFAULT_HEAL_RATIO,
            value_parser = crate::cmd::browser::parse_heal_ratio
        )]
        max_heal_ratio: f32,
        /// Check the run against the profile allowlist without launching a browser.
        #[arg(long)]
        dry_run: bool,
    },
    /// Log in once in a headed browser and save the session as an encrypted profile.
    Auth {
        /// Profile name; used as a path component under browser/profiles.
        site: String,
        /// Login page to open in a headed Playwright session.
        #[arg(long)]
        url: String,
        /// Replace an existing profile state after a fresh login.
        #[arg(long)]
        reauth: bool,
        /// Browser engine to use (Chrome, Chromium, Firefox, or Edge).
        /// Without this flag, MUR detects installed engines and asks when
        /// more than one is available.
        #[arg(long, value_enum)]
        browser: Option<BrowserEngine>,
        /// Domain this profile may navigate to (repeatable). Subdomains are
        /// included. Defaults to the host of `--url`.
        #[arg(long = "allow-domain", value_name = "DOMAIN")]
        allow_domain: Vec<String>,
    },
    /// Run the secret broker (needs MUR_BROWSER_BROKER_TOKEN; `record` starts its own).
    Broker,
    /// List recorded runs as a table (name, summary, domain, steps, mode, runs, date).
    List {
        /// Machine-readable JSON array (every field, nothing truncated).
        #[arg(long, conflicts_with_all = ["tsv", "oneline"])]
        json: bool,
        /// Tab-separated values, one run per line.
        #[arg(long, conflicts_with = "oneline")]
        tsv: bool,
        /// Names only, one per line (for scripts and shell completion).
        #[arg(long)]
        oneline: bool,
        /// Omit the header line (table and --tsv).
        #[arg(long)]
        no_header: bool,
        /// Only runs carrying this tag (repeat to require several).
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,
        /// Only runs recorded with this site profile.
        #[arg(long)]
        profile: Option<String>,
        /// Only runs whose name, description, tags, domain or any step intent matches this regex.
        #[arg(long, value_name = "RE")]
        grep: Option<String>,
        /// Only runs recorded since: 7d, 24h, or YYYY-MM-DD.
        #[arg(long, value_name = "WHEN")]
        since: Option<String>,
        /// Order: name (default), recent, or frecency (most-and-latest replayed first).
        #[arg(long, value_enum, default_value = "name")]
        sort: crate::cmd::browser::list::Sort,
    },
    /// Show one recorded run.
    Show { name: String },
    /// Export a recorded run as a Playwright `.spec.ts` file.
    Export {
        name: String,
        /// Write the spec to this path instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// List saved profiles and recorded runs (does not check the toolchain; see `doctor`).
    Status,
    /// Check that record/replay can run: npx and node work, and a Playwright
    /// Chromium is installed. Read-only — prints the install command for
    /// anything missing and exits non-zero.
    Doctor {
        /// Also launch the pinned @playwright/mcp headless and render a local
        /// JS-only page (loopback only; may download the package on first run).
        #[arg(long)]
        live: bool,
    },
    /// Install the Chromium replay needs, grant the spawn permissions a
    /// browser run requires, then prove it renders. Prints the exact
    /// `npx … install-browser --only-shell chromium` command and the
    /// `mur agent perm …` commands, and runs each only if you type `yes`.
    /// Interactive only; exits non-zero unless the live test passes.
    Setup {
        /// Agent to grant the browser spawn permissions to. Defaults to
        /// `$MUR_AGENT`, so `mur browser setup` inside murmur grants the
        /// agent you are talking to.
        #[arg(long)]
        agent: Option<String>,
        /// Consent up front instead of being asked. Required when there is
        /// no terminal to prompt on, which is the case inside murmur. Every
        /// step still prints what it does before doing it.
        #[arg(long)]
        yes: bool,
    },
    /// Delete old recorded runs, keeping the most recently recorded ones.
    Prune {
        /// Number of most recent runs to keep.
        #[arg(long, default_value_t = 10)]
        keep: usize,
        /// Only delete runs older than this many days (beyond `--keep`).
        #[arg(long)]
        older_than: Option<u32>,
        /// Print what would be deleted without deleting anything.
        #[arg(long)]
        dry_run: bool,
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
        /// After provisioning, also let each worker's gateway SPAWN the render
        /// browser (`agent-browser`), resolved to an absolute path — the exec
        /// allowlist searches directories that exclude a user's npm prefix, so
        /// the bare name grants nothing.
        ///
        /// Separate consent from `--grant-egress`: that one is "may it reach
        /// the web", this is "may it execute a browser inside the sandbox".
        /// The wizard asks them as two questions; this is that second answer
        /// for scripted use.
        #[arg(long)]
        grant_browser: bool,
        /// Denied host (repeatable) for the `--grant-egress` grant; ignored
        /// otherwise.
        #[arg(long = "deny-host")]
        deny_hosts: Vec<String>,
        /// Skip the `--grant-egress` consent prompt. Use for scripted /
        /// non-interactive grants.
        #[arg(long)]
        yes: bool,
        /// Render engine for the worker's gateway `fetch` (tier 2/3). `obscura`
        /// grants exec for the obscura binaries at `~/.mur/aura/` and prints how
        /// to enable it in the gateway config. Default: `agent-browser` (unchanged).
        #[arg(long)]
        render_engine: Option<String>,
    },
    /// Interactive first-time setup: model, worker count, budget, egress consent
    Setup,
    /// Check the render browser: report what is installed, version-check it,
    /// and print the install commands if nothing is found. Read-only — never
    /// installs; exits non-zero when no render browser runs.
    Doctor {
        /// Also render a local JS-only page with the engine the gateway would
        /// pick (loopback only; up to 30s on a cold browser)
        #[arg(long)]
        render: bool,
    },
    /// Show the status panel (same as bare `mur deep-research`). Without this
    /// variant the word `status` parsed as a one-word research question and
    /// dispatched a fleet run titled "status".
    Status,
    /// Store a search-provider API key in the OS keychain and point
    /// `~/.mur/config.yaml` at it.
    ///
    /// The key is read from the terminal without echo (or from stdin when
    /// piped) and is NEVER taken as an argument — argv is visible to every
    /// process via `ps` and lands in shell history. Only a
    /// `keychain:mur/<provider>` reference is written to config.yaml; the
    /// secret itself never enters the file.
    Secret {
        /// Use Brave Search (the default when no provider flag is given).
        #[arg(long, group = "search_provider")]
        brave: bool,
        /// Use Tavily.
        #[arg(long, group = "search_provider")]
        tavily: bool,
        /// Use SerpApi.
        #[arg(long = "serpapi", group = "search_provider")]
        serp_api: bool,
        /// Use Firecrawl.
        #[arg(long, group = "search_provider")]
        firecrawl: bool,
        /// Remove the stored key and drop its reference from config.yaml.
        #[arg(long)]
        clear: bool,
        /// List which providers currently have a key configured (no key is
        /// ever printed — only whether one is present).
        #[arg(long)]
        list: bool,
    },
    /// Run a deep-research fleet's guarded loop (thin wrapper over
    /// `mur fleet run --loop` — see `cmd/fleet/loop_run.rs`). This drives
    /// only the loop's bounds (deadline / stuck / cost_usd /
    /// kill-switch / marker convergence); it does NOT reimplement or
    /// bypass anything the plain fleet loop already does.
    Run {
        /// Fleet name (as created by `mur fleet create`, typically after
        /// `mur deep-research provision`)
        name: String,
        /// Ignored since 2.79 (kept for old scripts); bounds are `mur fleet limits`
        #[arg(long)]
        max_iterations: Option<u32>,
        /// Wall-clock deadline, e.g. 30s/5m/2h (overrides fleet.yaml)
        #[arg(long)]
        deadline: Option<String>,
        /// Legacy spelling of `limits.cost_usd` — prefer `mur fleet limits <name> --cost-usd`
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
