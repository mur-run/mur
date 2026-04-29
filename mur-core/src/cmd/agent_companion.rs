//! `mur agent companion ...` subcommands (Phase 1.1).
use clap::{Args, Subcommand};
use content::ContentArgs;
use proactive::ProactiveArgs;
use quiet::QuietArgs;
use templates::TemplatesArgs;
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
    /// Manage the per-agent content pool (add entries to situation files).
    Content(ContentArgs),
    /// Enable or disable proactive (companion-initiated) messages.
    Proactive(ProactiveArgs),
    /// Pause or clear proactive messages for a duration or until a timestamp.
    Quiet(QuietArgs),
    /// Eject embedded voice templates to disk for editing.
    Templates(TemplatesArgs),
    /// Write, re-compose, or diff the agent's voice.md.
    Voice(VoiceArgs),
    /// List inbox messages for an agent.
    Inbox(inbox::InboxArgs),
    /// Acknowledge an inbox message with a signal (--good / --bad / --dismiss).
    Ack(inbox::AckArgs),
}

pub async fn run(args: CompanionArgs) -> anyhow::Result<()> {
    match args.cmd {
        CompanionCmd::Init {
            name,
            answers,
            re_init,
        } => init::run(&name, answers, re_init).await,
        CompanionCmd::Content(args) => content::run(args).await,
        CompanionCmd::Proactive(args) => proactive::run(args).await,
        CompanionCmd::Quiet(args) => quiet::run(args).await,
        CompanionCmd::Templates(args) => templates::run(args).await,
        CompanionCmd::Voice(args) => voice::run(args).await,
        CompanionCmd::Inbox(a) => inbox::run_inbox(a).await,
        CompanionCmd::Ack(a) => inbox::run_ack(a).await,
    }
}

mod content;
mod inbox;
mod init;
mod proactive;
mod quiet;
mod templates;
mod util;
mod voice;
