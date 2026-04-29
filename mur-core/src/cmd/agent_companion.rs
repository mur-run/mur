//! `mur agent companion ...` subcommands (Phase 1.1).
use clap::{Args, Subcommand};
use proactive::ProactiveArgs;
use quiet::QuietArgs;
use voice::VoiceArgs;

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
    /// Pause or clear proactive messages for a duration or until a timestamp.
    Quiet(QuietArgs),
    /// Write, re-compose, or diff the agent's voice.md.
    Voice(VoiceArgs),
}

pub async fn run(args: CompanionArgs) -> anyhow::Result<()> {
    match args.cmd {
        CompanionCmd::Init {
            name,
            answers,
            re_init,
        } => init::run(&name, answers, re_init).await,
        CompanionCmd::Proactive(args) => proactive::run(args).await,
        CompanionCmd::Quiet(args) => quiet::run(args).await,
        CompanionCmd::Voice(args) => voice::run(args).await,
    }
}

mod init;
mod proactive;
mod quiet;
mod util;
mod voice;
