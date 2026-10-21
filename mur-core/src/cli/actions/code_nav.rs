use super::*;

#[derive(Subcommand)]
pub enum CodeNavAction {
    /// Install ast-grep (and, with --with-serena, serena) for an agent.
    /// Prints the install, permission and language-server tables, then
    /// applies them only after you type `yes` (or pass --yes). Re-runs ask
    /// only about what changed since the last successful setup. If setup
    /// fails partway, nothing is recorded, so the next run asks again about
    /// everything, including steps that already succeeded.
    ///
    /// With --with-serena, setup then starts each enabled language server
    /// under the agent's sandbox and reports whether it answered
    /// `initialize`. Without --with-serena no language server is started,
    /// so nothing is smoke-tested. A failed smoke launch keeps the installs
    /// and grants already recorded.
    Setup {
        /// Agent to set up (case-insensitive).
        #[arg(long, required_unless_present = "wizard")]
        agent: Option<String>,
        /// Guided setup for one or more agents (positional names): pick
        /// projects from `mur project list`, detect their language servers,
        /// and apply one combined plan after one confirmation. Then offers
        /// to restart the agents that changed.
        #[arg(
            long,
            requires = "agents",
            conflicts_with_all = ["agent", "with_serena", "project", "entry", "lsp", "lsp_only", "no_ast_grep"]
        )]
        wizard: bool,
        /// With --wizard: the agents to set up (case-insensitive).
        #[arg(value_name = "AGENT", requires = "wizard")]
        agents: Vec<String>,
        /// With --wizard: a project directory to add, indexed or not
        /// (repeatable). The filesystem root, your home directory itself and
        /// anything under MUR's home are refused.
        #[arg(long = "path", value_name = "DIR", requires = "wizard")]
        paths: Vec<std::path::PathBuf>,
        /// With --wizard: restart the running agents that changed, without
        /// asking. Without it, --yes only grants and prints the restart
        /// command.
        #[arg(long, requires = "wizard")]
        restart: bool,
        /// Also set up serena (LSP-backed symbol navigation). Off by default.
        #[arg(long)]
        with_serena: bool,
        /// The repository serena serves. Required with --with-serena.
        #[arg(long, value_name = "DIR")]
        project: Option<std::path::PathBuf>,
        /// Name of the serena MCP entry to create or update. Each entry has
        /// its own SERENA_HOME, so give a second repository its own name
        /// (`[a-z0-9_-]`, lower case, up to 64 characters, no trailing or doubled `_`). Defaults to `serena`.
        #[arg(long, value_name = "NAME", default_value = "serena")]
        entry: String,
        /// Skip ast-grep (installed by default).
        #[arg(long)]
        no_ast_grep: bool,
        /// Language server to enable (repeatable). Replaces the default set
        /// (Python, PHP). Lua and C/C++, which serena downloads itself, and
        /// the high-risk languages are enabled only when named here.
        #[arg(long, value_name = "LANG", conflicts_with = "lsp_only")]
        lsp: Vec<String>,
        /// Set this entry's language servers to exactly these (repeatable),
        /// dropping any it listed before. Rewrites `project.yml` to match.
        /// Use it to remove a language whose server cannot start.
        #[arg(long, value_name = "LANG")]
        lsp_only: Vec<String>,
        /// Confirm the printed plan without a prompt. Never adds to it.
        #[arg(long)]
        yes: bool,
    },
    /// Check that what `setup` recorded still matches reality: installs on
    /// disk, grants in the agent profile, each entry's `project.yml`
    /// languages, and code-nav grants added by hand. Offline and read-only;
    /// prints the fix for each warning but changes nothing.
    Doctor {
        /// Agent to check (case-insensitive). Defaults to every agent that
        /// has run setup.
        #[arg(long)]
        agent: Option<String>,
    },
    /// Internal: seal this process as the agent, start one language server,
    /// and report whether it answered `initialize` (setup's smoke launch).
    #[command(name = "smoke-ls", hide = true)]
    SmokeLs {
        #[arg(long)]
        agent: String,
        #[arg(long, value_name = "DIR")]
        project: std::path::PathBuf,
        /// The server's argv.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
}
