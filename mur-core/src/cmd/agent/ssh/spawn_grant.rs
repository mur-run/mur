//! Plan O3: `ssh` itself must be spawnable for any of this to be used.
//!
//! When it is not, offer to add the grant (attended, literal `yes`). A run
//! with no terminal never auto-grants — not even with `--yes`, which covers
//! the ssh signing grant only — and prints the command instead.

use std::io::{BufRead, Write};

use anyhow::Result;

use super::super::load_profile_for_edit;
use super::super::perm::grant_spawn;
use crate::cmd::consent::literal_yes;

/// The binary and the rule live in `mur-common` so the runtime's
/// `ssh_request` tool answers with the same verdict.
pub use mur_ssh_request::{SSH_BINARY as SSH_SPAWN_BINARY, ssh_spawnable};

/// The manual command, printed whenever the grant is not added here.
pub fn manual_command(agent: &str) -> String {
    format!("mur agent perm allow-spawn {agent} {SSH_SPAWN_BINARY}")
}

/// What the offer did.
#[derive(Debug, PartialEq, Eq)]
pub enum Offer {
    AlreadySpawnable,
    Granted,
    Declined,
    /// No terminal: told the user the command.
    PrintedCommand,
}

/// Pure decision + prompt over the given streams; `grant` runs only on yes.
pub fn decide(
    agent: &str,
    spawnable: bool,
    attended: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    grant: impl FnOnce() -> Result<()>,
) -> Result<Offer> {
    if spawnable {
        return Ok(Offer::AlreadySpawnable);
    }
    let cmd = manual_command(agent);
    if !attended {
        writeln!(
            out,
            "note: '{agent}' may not exec `{SSH_SPAWN_BINARY}` yet, so it cannot use this grant. \
             Add it with:\n  {cmd}"
        )?;
        return Ok(Offer::PrintedCommand);
    }
    writeln!(
        out,
        "'{agent}' may not exec `{SSH_SPAWN_BINARY}` yet. Add it to the spawn allowlist now \
         (same as `{cmd}`)?"
    )?;
    if literal_yes(input, out)? {
        grant()?;
        writeln!(
            out,
            "added `{SSH_SPAWN_BINARY}` to '{agent}' spawn allowlist"
        )?;
        Ok(Offer::Granted)
    } else {
        writeln!(out, "left unchanged; to add it later:\n  {cmd}")?;
        Ok(Offer::Declined)
    }
}

/// [`decide`] on the real terminal. `--yes` runs are treated as unattended.
pub fn offer(agent: &str, yes: bool) -> Result<Offer> {
    use std::io::IsTerminal;
    let (_, profile) = load_profile_for_edit(agent)?;
    let attended = !yes && std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    decide(
        agent,
        ssh_spawnable(&profile.entitlements.processes),
        attended,
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
        // An ordinary pinned save: it takes the profile lock itself and is
        // refused if an ssh write landed after it loaded.
        || grant_spawn(agent, SSH_SPAWN_BINARY),
    )
}
