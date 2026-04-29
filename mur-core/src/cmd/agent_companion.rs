//! `mur agent companion ...` subcommands (Phase 1.1).
use clap::{Args, Subcommand};
use proactive::ProactiveArgs;

#[derive(Args, Debug)]
pub struct CompanionArgs {
    #[command(subcommand)]
    pub cmd: CompanionCmd,
}

#[derive(Subcommand, Debug)]
pub enum CompanionCmd {
    /// Run onboarding wizard.
    Init {
        name: String,
        #[arg(long)]
        answers: Option<std::path::PathBuf>,
        #[arg(long)]
        re_init: bool,
    },
    /// Enable or disable proactive (companion-initiated) messages.
    Proactive(ProactiveArgs),
}

pub async fn run(args: CompanionArgs) -> anyhow::Result<()> {
    match args.cmd {
        CompanionCmd::Init {
            name,
            answers,
            re_init,
        } => init::run(&name, answers, re_init).await,
        CompanionCmd::Proactive(args) => proactive::run(args).await,
    }
}

mod init;
mod proactive;
mod util;
