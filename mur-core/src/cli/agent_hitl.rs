//! `mur agent hitl ...` — gate B's remembered approval decisions (#1806).

use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum AgentHitlAction {
    /// List the approval decisions the agent is currently replaying without
    /// asking (newest first): verdict, what was decided, age, expiry.
    List {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Forget remembered decisions so the agent asks again. Appends a signed
    /// revocation; nothing is deleted. Needs the agent running.
    Forget {
        /// Hash (or unique prefix) from `mur agent hitl list`
        #[arg(
            required_unless_present_any = ["all", "denies_only"],
            conflicts_with_all = ["all", "denies_only"]
        )]
        hash: Option<String>,
        /// Forget every remembered decision
        #[arg(long, conflicts_with = "denies_only")]
        all: bool,
        /// Forget only remembered denials; remembered allows keep replaying
        #[arg(long)]
        denies_only: bool,
    },
}
