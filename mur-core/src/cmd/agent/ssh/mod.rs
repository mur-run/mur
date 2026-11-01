//! `mur agent ssh allow|revoke|list` — the only writer of `entitlements.ssh`.
//!
//! Every write is one unit: `profile.yaml`, the entitlement pin, and
//! `<agent home>/ssh/known_hosts` all change, or none does. A profile written
//! without its pin would make the supervisor refuse to start, and a stale
//! `known_hosts` would point ssh at host keys the policy no longer allows.

mod commit;
mod edit;
mod resolve;
mod spawn_grant;
mod view;

use std::io::Write;

use anyhow::{Result, bail};

use super::{load_profile_for_edit, perm::warn_if_running, resolve_mur_home};
use crate::cmd::consent::{Consent, literal_yes};
use edit::{AllowOutcome, Grant, HostGrant, RevokeScope};
use resolve::{DEFAULT_SSH_PORT, KeyRef, ScannedKey};

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

/// The host keys an `allow` will add, after any scan.
fn host_grants(a: &AllowArgs) -> Result<Vec<HostGrant>> {
    let label = a.host_label.map(str::to_string);
    let pinned_fp = a
        .host_key
        .map(|fp| checked_fp("--host-key", fp))
        .transpose()?;
    let Some(host) = a.host else {
        if a.port.is_some() {
            bail!("--port only applies with --host");
        }
        return Ok(pinned_fp
            .map(|fp| HostGrant {
                host_key_fp: fp,
                label,
                known_host: None,
            })
            .into_iter()
            .collect());
    };
    let scanned = resolve::scan_host(host, a.port.unwrap_or(DEFAULT_SSH_PORT))?;
    let keep: Vec<ScannedKey> = match &pinned_fp {
        Some(fp) => {
            let hit: Vec<ScannedKey> = scanned
                .iter()
                .filter(|k| &k.fingerprint == fp)
                .cloned()
                .collect();
            if hit.is_empty() {
                let offered: Vec<&str> = scanned.iter().map(|k| k.fingerprint.as_str()).collect();
                bail!(
                    "{host} did not offer host key {fp}; it offered:\n  {}\n\
                     Nothing was changed. If the server rotated its key, verify the new one \
                     out of band before allowing it.",
                    offered.join("\n  ")
                );
            }
            hit
        }
        None => scanned,
    };
    let label = label.or_else(|| Some(host.to_string()));
    Ok(keep
        .into_iter()
        .map(|k| HostGrant {
            host_key_fp: k.fingerprint,
            label: label.clone(),
            known_host: Some(k.entry),
        })
        .collect())
}

pub fn cmd_ssh_allow(a: &AllowArgs) -> Result<()> {
    check_yes_contract(a)?;
    let consent = Consent::detect(a.yes);
    if consent == Consent::Impossible {
        bail!(
            "`mur agent ssh allow` lets the agent sign with your ssh key and needs confirmation, \
             but there is no terminal to ask. Re-run at a terminal, or with \
             --key <SHA256:...> --host-key <SHA256:...> --yes"
        );
    }
    let agent = canonical_agent(a.agent)?;
    // Fail on a missing agent before any network lookup.
    load_profile_for_edit(&agent)?;
    let key = resolve::resolve_key(a.key)?;
    let hosts = host_grants(a)?;

    let mut err = std::io::stderr();
    print_preview(&mut err, &agent, &key, a.label, &hosts)?;
    if consent == Consent::Ask && !literal_yes(&mut std::io::stdin().lock(), &mut err)? {
        bail!("cancelled — nothing was changed");
    }

    let key_label = a.label.map(str::to_string);
    let key_fp = key.fingerprint().to_string();
    let grants: Vec<Grant> = if hosts.is_empty() {
        vec![Grant {
            key_fp: key_fp.clone(),
            key_label: key_label.clone(),
            host: None,
        }]
    } else {
        hosts
            .iter()
            .map(|h| Grant {
                key_fp: key_fp.clone(),
                key_label: key_label.clone(),
                host: Some(h.clone()),
            })
            .collect()
    };
    let mut outcome = AllowOutcome::Updated;
    commit::apply(&agent, |ent| {
        for g in &grants {
            if edit::allow(ent, g) == AllowOutcome::Added {
                outcome = AllowOutcome::Added;
            }
        }
        Ok(())
    })?;

    let verb = match outcome {
        AllowOutcome::Added => "allowed",
        AllowOutcome::Updated => "already allowed (labels refreshed)",
    };
    println!(
        "{agent}: {verb} key {key_fp} for {} host key(s)",
        hosts.len()
    );
    print_host_notes(&hosts);
    spawn_grant::offer(&agent, a.yes)?;
    warn_if_running(&agent);
    Ok(())
}

fn print_preview(
    out: &mut dyn Write,
    agent: &str,
    key: &KeyRef,
    label: Option<&str>,
    hosts: &[HostGrant],
) -> Result<()> {
    writeln!(
        out,
        "Allow agent '{agent}' to request signatures from your ssh-agent:"
    )?;
    match key {
        KeyRef::Fingerprint(fp) => writeln!(out, "  key       {fp}")?,
        KeyRef::Resolved {
            fingerprint,
            comment,
        } => writeln!(
            out,
            "  key       {fingerprint}  (matched comment '{comment}')"
        )?,
    }
    if let Some(l) = label {
        writeln!(out, "  label     {l}  (display only)")?;
    }
    if hosts.is_empty() {
        writeln!(
            out,
            "  hosts     none — the key is listed but signs for no host yet"
        )?;
    }
    for h in hosts {
        let how = match &h.known_host {
            Some(kh) => format!("{} {}, scanned just now", kh.host, kh.key_type),
            None => "fingerprint given, not scanned".to_string(),
        };
        writeln!(out, "  host key  {}  ({how})", h.host_key_fp)?;
    }
    writeln!(
        out,
        "Every signature still asks you first (HITL), unless you remember a decision."
    )?;
    if hosts.iter().any(|h| h.known_host.is_some()) {
        writeln!(
            out,
            "Compare the host key(s) with the fingerprints the server operator publishes \
             before you confirm."
        )?;
    }
    Ok(())
}

fn print_host_notes(hosts: &[HostGrant]) {
    if hosts.iter().any(|h| h.known_host.is_none()) {
        println!(
            "note: --host-key alone writes no known_hosts entry, so the agent's ssh will fail \
             host key checking for that host. Add --host <name> to write one (the scanned key \
             is checked against --host-key)."
        );
    }
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
