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
        #[arg(long)]
        agent: String,
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
        /// (Python, PHP, Lua, C/C++); high-risk languages need this.
        #[arg(long, value_name = "LANG")]
        lsp: Vec<String>,
        /// Confirm the printed plan without a prompt. Never adds to it.
        #[arg(long)]
        yes: bool,
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
