//! `mur agent companion ...` subcommands (Phase 1.1).
use clap::{Args, Subcommand};
use content::ContentArgs;
use preview::PreviewArgs;
use proactive::ProactiveArgs;
use quiet::QuietArgs;
use rhythm::RhythmArgs;
use templates::TemplatesArgs;
use voice::VoiceArgs;
use why::WhyArgs;

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
    /// Preview a companion message for a given situation (read-only).
    Preview(PreviewArgs),
    /// Explain why a companion message was sent (or list last 7 days of sent messages).
    #[command(name = "why-did-you-message")]
    WhyDidYouMessage(WhyArgs),
    /// Manage companion rhythm state (wipe inbox/ledger/bandit, preserve voice config).
    Rhythm(RhythmArgs),
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
        CompanionCmd::Preview(args) => preview::run(args).await,
        CompanionCmd::WhyDidYouMessage(args) => why::run(args).await,
        CompanionCmd::Rhythm(args) => rhythm::run(args).await,
    }
}

pub mod card;
mod content;
mod inbox;
pub mod init;
mod preview;
mod proactive;
mod quiet;
mod rhythm;
mod templates;
mod util;
mod voice;
mod why;
