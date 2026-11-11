//! `mur agent ssh list`: what the agent may sign for, and for which hosts.
//!
//! Labels are display only and say so; fingerprints are what the proxy
//! matches. `--json` prints the `entitlements.ssh` block itself.

use anyhow::{Context, Result};
use mur_common::agent::SshEntitlement;

use super::super::{load_profile_for_edit, resolve_mur_home};
use super::edit::Removed;
use super::spawn_grant::{manual_command, ssh_spawnable};
use super::status::{PairState, SshStatus};

pub fn cmd_ssh_list(agent: &str, json: bool) -> Result<()> {
    let agent = super::canonical_agent(agent)?;
    let (_, profile) = load_profile_for_edit(&agent)?;
    let ssh = &profile.entitlements.ssh;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(ssh).context("serialize entitlements.ssh")?
        );
        return Ok(());
    }
    print!("{}", render(&agent, ssh));
    let st = super::status::status(&resolve_mur_home()?, &agent, ssh);
    print!("{}", render_status(&st));
    if !ssh.is_empty() && !ssh_spawnable(&profile.entitlements.processes) {
        println!(
            "note: '{agent}' may not exec ssh yet; add it with `{}`",
            manual_command(&agent)
        );
    }
    Ok(())
}

/// Human-readable listing.
pub fn render(agent: &str, ssh: &SshEntitlement) -> String {
    if ssh.is_empty() {
        return format!(
            "'{agent}' may not use any ssh key (default deny).\n\
             Grant one with `mur agent ssh allow {agent} --key <SHA256:...> --host <name>`.\n"
        );
    }
    let mut out = format!("ssh keys '{agent}' may sign with (labels are display only):\n");
    for k in &ssh.keys {
        out.push_str(&format!("  key {}{}\n", k.fingerprint, label(&k.label)));
        if k.allow_hosts.is_empty() {
            out.push_str("    (no hosts — signs for nothing)\n");
        }
        for h in &k.allow_hosts {
            let kh = if h.known_hosts.is_empty() {
                "no known_hosts entry".to_string()
            } else {
                let names: Vec<&str> = h.known_hosts.iter().map(|k| k.host.as_str()).collect();
                format!("known_hosts: {}", names.join(", "))
            };
            out.push_str(&format!(
                "    host {}{}  [{kh}]\n",
                h.host_key,
                label(&h.label)
            ));
        }
    }
    out
}

/// Where each granted pair stands, and what the agent has asked for. Says
/// "signed", never "connected": a mark proves the proxy returned a signature
/// under the approval, not that the login worked.
pub fn render_status(st: &SshStatus) -> String {
    let mut out = String::new();
    if st.pairs.is_empty() && st.requests.is_empty() {
        return out;
    }
    if let Some(why) = &st.decisions_unverified {
        out.push_str(&format!(
            "approvals: none trusted ({why}); see `mur channel repair`\n"
        ));
    }
    if !st.pairs.is_empty() {
        out.push_str("approval state:\n");
    }
    for p in &st.pairs {
        let state = match &p.state {
            PairState::Signed { at } => format!("signed under your approval ({})", ts(at)),
            PairState::Approved => "approved, no signing seen yet".into(),
            PairState::NotApproved => "not approved".into(),
        };
        out.push_str(&format!(
            "  key {} host {}: {state}\n",
            p.key_fp, p.host_key_fp
        ));
        if let Some(at) = &p.last_expired_mark {
            out.push_str(&format!(
                "    last signed {}, under an approval that has since expired\n",
                ts(at)
            ));
        }
    }
    let open: Vec<_> = st
        .requests
        .iter()
        .filter(|r| !r.cleared(&st.pairs))
        .collect();
    if !open.is_empty() {
        out.push_str("requested by the agent (its own words, unverified):\n");
    }
    for r in open {
        let q = &r.request;
        let user = q
            .user
            .as_deref()
            .map(|u| format!("{u}@"))
            .unwrap_or_default();
        let granted = if r.pairs.is_empty() {
            "not granted"
        } else {
            "granted"
        };
        out.push_str(&format!(
            "  {user}{}  [{granted}]  {}\n",
            q.target.canonical(),
            q.reason
        ));
    }
    out
}

fn ts(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn label(l: &Option<String>) -> String {
    l.as_deref().map(|l| format!("  ({l})")).unwrap_or_default()
}

/// One line describing a revoked grant.
pub fn removed_line(r: &Removed) -> String {
    match &r.host_key_fp {
        Some(h) => format!("key {} for host {h}", r.key_fp),
        None => format!("key {} (no hosts)", r.key_fp),
    }
}
