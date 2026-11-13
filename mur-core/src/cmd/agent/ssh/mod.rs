//! `mur agent ssh allow|revoke|list` — the only writer of `entitlements.ssh`.
//!
//! Every write is one unit: `profile.yaml`, the entitlement pin, and
//! `<agent home>/ssh/known_hosts` all change, or none does. A profile written
//! without its pin would make the supervisor refuse to start, and a stale
//! `known_hosts` would point ssh at host keys the policy no longer allows.

mod allow;
mod commit;
pub(in crate::cmd::agent) mod edit;
pub(in crate::cmd::agent) mod resolve;
pub mod setup;
mod spawn_grant;
mod stage;
pub mod status;
pub(in crate::cmd::agent) mod view;

use std::io::Write;

use anyhow::{Result, bail};

use super::{load_profile_for_edit, perm::warn_if_running, resolve_mur_home};
use crate::cmd::consent::{Consent, literal_yes};
use edit::RevokeScope;

pub use allow::cmd_ssh_allow;
pub use view::cmd_ssh_list;

/// Parsed `mur agent ssh allow` arguments.
pub struct AllowArgs<'a> {
    pub agent: &'a str,
    pub key: &'a str,
    pub label: Option<&'a str>,
    pub host: Option<&'a str>,
    pub port: Option<u16>,
    pub host_key: Option<&'a str>,
    pub host_label: Option<&'a str>,
    pub yes: bool,
    /// Write the grant and stop there; the user restarts later.
    pub no_restart: bool,
}

/// The `--yes` contract: "I already know both fingerprints, don't ask me".
/// It never skips a check, so it cannot be combined with anything the CLI
/// would have to look up and show first.
fn check_yes_contract(a: &AllowArgs) -> Result<()> {
    if !a.yes {
        return Ok(());
    }
    if a.host.is_some() {
        bail!(
            "--yes cannot be combined with --host: --yes never looks up a host key for you. \
             Pass --host-key <SHA256:...> you verified out of band, or drop --yes and confirm \
             the scanned key at a terminal"
        );
    }
    if a.host_key.is_none() {
        bail!("--yes needs --host-key <SHA256:...>: a key with no host signs for nothing");
    }
    if !mur_common::agent::is_fingerprint(a.key) {
        bail!(
            "--yes needs --key as a SHA256: fingerprint; a key comment is a lookup that must \
             be confirmed at a terminal"
        );
    }
    Ok(())
}

fn canonical_agent(typed: &str) -> Result<String> {
    let home = resolve_mur_home()?;
    Ok(crate::a2a_dial::canonicalize_agent_name(&home, typed))
}

fn checked_fp(flag: &str, fp: &str) -> Result<String> {
    if !mur_common::agent::is_fingerprint(fp) {
        bail!("{flag} '{fp}' is not a SHA256: fingerprint (as printed by `ssh-keygen -lf`)");
    }
    Ok(fp.to_string())
}

/// Parsed `mur agent ssh revoke` arguments.
pub struct RevokeArgs<'a> {
    pub agent: &'a str,
    pub key: Option<&'a str>,
    pub host_key: Option<&'a str>,
    pub yes: bool,
}

pub fn cmd_ssh_revoke(a: &RevokeArgs) -> Result<()> {
    let key = a.key.map(|k| checked_fp("--key", k)).transpose()?;
    let host_key = a
        .host_key
        .map(|k| checked_fp("--host-key", k))
        .transpose()?;
    let scope = RevokeScope::new(key, host_key)?;
    let agent = canonical_agent(a.agent)?;
    let (_, profile) = load_profile_for_edit(&agent)?;
    let preview = edit::revoke_preview(&profile.entitlements.ssh, &scope);
    if preview.is_empty() {
        bail!("nothing matched in '{agent}'; see `mur agent ssh list {agent}`");
    }
    if scope.is_batch() {
        let mut err = std::io::stderr();
        writeln!(
            err,
            "This revokes {} grant(s) from '{agent}':",
            preview.len()
        )?;
        for r in &preview {
            writeln!(err, "  {}", view::removed_line(r))?;
        }
        match Consent::detect(a.yes) {
            Consent::Yes => {}
            Consent::Impossible => bail!(
                "a batch revoke needs confirmation and there is no terminal; review the list \
                 above and re-run with --yes"
            ),
            Consent::Ask => {
                if !literal_yes(&mut std::io::stdin().lock(), &mut err)? {
                    bail!("cancelled — nothing was changed");
                }
            }
        }
    }
    let mut removed = Vec::new();
    commit::apply(&agent, |ent| {
        removed = edit::revoke(ent, &scope)?;
        Ok(())
    })?;
    for r in &removed {
        println!("revoked {}", view::removed_line(r));
    }
    warn_if_running(&agent);
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "commit_tests.rs"]
mod commit_tests;
