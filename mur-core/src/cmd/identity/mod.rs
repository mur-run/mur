//! `mur identity` — the human owner's signing identity (at-mention phase 2, A2).
//!
//! Every subcommand needs the human's own terminal: the private half lives in
//! `keys/.human/`, which no agent sandbox can read or write.

mod init;
mod rekey;
mod show;

use std::path::Path;

use anyhow::Result;
use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum IdentityAction {
    /// Create the human owner identity if there is none (idempotent)
    Init,
    /// Show the owner's public key, key version and file locations
    Show {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Rotate the owner key; the attestation is signed by the old key
    Rekey {
        /// scheduled | suspect-compromise | owner-change
        #[arg(long, default_value = "scheduled")]
        reason: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

pub fn run(mur_home: &Path, action: IdentityAction) -> Result<()> {
    match action {
        IdentityAction::Init => init::run(mur_home),
        IdentityAction::Show { json } => show::run(mur_home, json),
        IdentityAction::Rekey { reason, yes } => rekey::run(mur_home, &reason, yes),
    }
}

#[cfg(test)]
mod tests;
