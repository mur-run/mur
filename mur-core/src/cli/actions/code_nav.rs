use super::*;

#[derive(Subcommand)]
pub enum CodeNavAction {
    /// Install ast-grep (and, with --with-serena, serena) for an agent.
    /// Prints the install, permission and language-server tables, then
    /// applies them only after you type `yes` (or pass --yes). Re-runs ask
    /// only about what changed since the last setup.
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
}
