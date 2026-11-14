//! `mur agent ssh allow`, rebuilt on `setup::plan` / `apply_and_restart`
//! (SSH setup spec, "`mur agent ssh allow` is rebuilt on these").

use std::io::Write;

use anyhow::{Result, bail};
use mur_ssh_request::RequestHost;

use super::edit::{AllowOutcome, HostGrant};
use super::resolve::{self, KeyRef};
use super::setup::probe::System;
use super::setup::verdict::{self, HostChoice};
use super::setup::{
    Applied, Approval, SetupPlan, SpawnChange, Verdict, apply_and_restart, life::AgentLifecycle,
    plan,
};
use super::spawn_grant::manual_command;
use super::{AllowArgs, canonical_agent, check_yes_contract, checked_fp, load_profile_for_edit};
use crate::cmd::consent::{Consent, literal_yes};

/// The host keys to grant, and the plan they came from (absent for a
/// fingerprint-only grant, which scans nothing).
struct Hosts {
    grants: Vec<HostGrant>,
    plan: Option<SetupPlan>,
}

pub fn cmd_ssh_allow(a: &AllowArgs) -> Result<()> {
    check_yes_contract(a)?;
    if a.host.is_none() && a.host_key.is_none() {
        bail!(
            "a key with no host signs for nothing: pass --host <name> (scanned and shown) or \
             --host-key <SHA256:...> you verified out of band. Nothing was changed."
        );
    }
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
    let (profile_path, profile) = load_profile_for_edit(&agent)?;
    let agent_home = profile_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    let key = resolve::resolve_key(a.key)?;
    let hosts = host_grants(a, &agent, &agent_home, &profile.entitlements)?;

    // `none` is not switched here: that needs the card's separate consent.
    // `--yes` never adds a spawn grant (it covers the signing grant only).
    let needed = SpawnChange::needed(&profile.entitlements.processes);
    let spawn = match needed {
        Some(SpawnChange::AddSsh) if consent == Consent::Ask => needed,
        _ => None,
    };

    let mut err = std::io::stderr();
    print_preview(&mut err, &agent, &key, a.label, &hosts, spawn, a.no_restart)?;
    if consent == Consent::Ask && !literal_yes(&mut std::io::stdin().lock(), &mut err)? {
        bail!("cancelled — nothing was changed");
    }

    let home = super::resolve_mur_home()?;
    let approval = Approval {
        key_fp: key.fingerprint().to_string(),
        key_label: a.label.map(str::to_string),
        hosts: hosts.grants.clone(),
        spawn,
        spawn_consented: false,
        restart: !a.no_restart,
    };
    let mut life = AgentLifecycle::new(&agent, &home);
    let applied = apply_and_restart(
        &home,
        &agent,
        profile.identity.key_version,
        &approval,
        &mut life,
    )?;
    report(&agent, &approval, &hosts, needed, &applied);
    Ok(())
}

/// The host keys to grant. With `--host`: plan, verdict, then the verdict
/// table decides which scanned keys (`--host-key` plays the paste).
fn host_grants(
    a: &AllowArgs,
    agent: &str,
    agent_home: &std::path::Path,
    ent: &mur_common::agent::Entitlements,
) -> Result<Hosts> {
    let label = a.host_label.map(str::to_string);
    let pasted = a
        .host_key
        .map(|fp| checked_fp("--host-key", fp))
        .transpose()?;
    let Some(host) = a.host else {
        if a.port.is_some() {
            bail!("--port only applies with --host");
        }
        return Ok(Hosts {
            grants: pasted
                .map(|fp| HostGrant {
                    host_key_fp: fp,
                    label,
                    known_host: None,
                })
                .into_iter()
                .collect(),
            plan: None,
        });
    };
    let target = RequestHost::parse(host, a.port).map_err(|e| anyhow::anyhow!("{e}"))?;
    let p = plan(&System, agent, agent_home, ent, target, None)?;
    // At a terminal, confirming the listed keys is the CLI's "I compared
    // this" for each of them; the card ticks keys one by one.
    let choice = HostChoice {
        ticked: if pasted.is_none() {
            p.scanned.iter().map(|k| k.fingerprint.clone()).collect()
        } else {
            Vec::new()
        },
        pasted,
    };
    if p.verdict == Verdict::Differs && choice.pasted.is_none() {
        bail!("{}", verdict::differs_text(&p.host));
    }
    let keep = verdict::select(&p.host, &p.scanned, &p.verdict, &p.matched, &choice)?;
    let label = label.or_else(|| Some(host.to_string()));
    Ok(Hosts {
        grants: keep
            .into_iter()
            .map(|k| HostGrant {
                host_key_fp: k.fingerprint,
                label: label.clone(),
                known_host: Some(k.entry),
            })
            .collect(),
        plan: Some(p),
    })
}

fn verdict_line(p: &SetupPlan) -> &'static str {
    match p.verdict {
        Verdict::Trusted => "matches your ~/.ssh/known_hosts",
        Verdict::Unknown { downgraded: false } => "not in your ~/.ssh/known_hosts; compare it",
        Verdict::Unknown { downgraded: true } => {
            "matches your ~/.ssh/known_hosts, but this agent can write that file, so the match \
             proves nothing; compare it"
        }
        Verdict::Differs => "your ~/.ssh/known_hosts stores a different key",
    }
}

fn print_preview(
    out: &mut dyn Write,
    agent: &str,
    key: &KeyRef,
    label: Option<&str>,
    hosts: &Hosts,
    spawn: Option<SpawnChange>,
    no_restart: bool,
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
    for h in &hosts.grants {
        let how = match (&h.known_host, &hosts.plan) {
            (Some(kh), Some(p)) => format!("{} {}, {}", kh.host, kh.key_type, verdict_line(p)),
            (Some(kh), None) => format!("{} {}", kh.host, kh.key_type),
            (None, _) => "fingerprint given, not scanned".to_string(),
        };
        writeln!(out, "  host key  {}  ({how})", h.host_key_fp)?;
    }
    if spawn == Some(SpawnChange::AddSsh) {
        writeln!(out, "  spawn     add `ssh` to the agent's spawn allowlist")?;
    }
    if no_restart {
        writeln!(
            out,
            "Not restarting (--no-restart): the first signature will ask you once."
        )?;
    } else {
        writeln!(
            out,
            "One restart. The first signature for these pairs will not ask again: confirming \
             here is that approval."
        )?;
    }
    if hosts
        .plan
        .as_ref()
        .is_some_and(|p| p.verdict != Verdict::Trusted)
    {
        writeln!(
            out,
            "Compare the host key(s) with the fingerprints the server operator publishes \
             before you confirm."
        )?;
    }
    Ok(())
}

fn report(
    agent: &str,
    a: &Approval,
    hosts: &Hosts,
    needed: Option<SpawnChange>,
    applied: &Applied,
) {
    let verb = match applied.outcome {
        AllowOutcome::Added => "allowed",
        AllowOutcome::Updated => "already allowed (labels refreshed)",
    };
    println!(
        "{agent}: {verb} key {} for {} host key(s)",
        a.key_fp,
        hosts.grants.len()
    );
    if applied.spawn.is_some() {
        println!("added `ssh` to '{agent}' spawn allowlist");
    }
    if hosts.grants.iter().any(|h| h.known_host.is_none()) {
        println!(
            "note: --host-key alone writes no known_hosts entry, so the agent's ssh will fail \
             host key checking for that host. Add --host <name> to write one (the scanned key \
             is checked against --host-key)."
        );
    }
    match (needed, applied.spawn) {
        (Some(SpawnChange::NoneToStrict), _) => {
            println!(
                "note: '{agent}' has spawn mode `none`, so it can run no program, ssh \
                 included. Approve the spawn change in murmur (`/ssh`) or Hub, where it is \
                 shown with its own consent. Switching to `strict` with `ssh`:"
            );
            if let Some(es) = hosts.plan.as_ref().and_then(|p| p.exec_surface.as_ref()) {
                print!("{}", es.consent_text());
            }
        }
        (Some(SpawnChange::AddSsh), None) => println!(
            "note: '{agent}' may not exec `ssh` yet. Add it with:\n  {}",
            manual_command(agent)
        ),
        _ => {}
    }
    if let Some(l) = applied.preapproval.line() {
        println!("{l}");
    }
    if let Some(l) = applied.restart.line(agent) {
        println!("{l}");
    }
}
