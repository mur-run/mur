//! Applying the decided plan (#2016 Decision 2): confirm once, install the
//! shared tools once, install every target, ask once about new exec lanes,
//! commit each target, smoke it, report one row per target, then offer to
//! restart the agents that changed. Every side effect goes through [`Ops`].

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Result, bail};

use super::ops::Ops;
use super::prompt::Prompt;
use super::{Decided, Hold, table};
use crate::cmd::browser::setup::Consent;

/// What happened to one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Nothing new; not touched.
    Unchanged,
    Skipped(String),
    Blocked(String),
    InstallFailed(String),
    LanesDeclined,
    CommitFailed(String),
    /// Committed, then the smoke launch failed: grants are recorded.
    SmokeFailed(String),
    Applied,
}

impl Outcome {
    pub(super) fn failed(&self) -> bool {
        !matches!(
            self,
            Outcome::Unchanged | Outcome::Skipped(_) | Outcome::Applied
        )
    }

    /// Something was committed for this target.
    fn committed(&self) -> bool {
        matches!(self, Outcome::Applied | Outcome::SmokeFailed(_))
    }

    /// May have left grants without a manifest record: restarting would
    /// make that state live, so the user decides.
    fn blocks_restart(&self) -> bool {
        matches!(
            self,
            Outcome::InstallFailed(_) | Outcome::LanesDeclined | Outcome::CommitFailed(_)
        )
    }
}

pub(super) fn apply<O: Ops>(
    d: &Decided,
    consent: Consent,
    restart: bool,
    prompt: &mut dyn Prompt,
    ops: &mut O,
    out: &mut dyn Write,
) -> Result<()> {
    table::print(d, out)?;
    let mut outcomes: Vec<Outcome> = d
        .specs
        .iter()
        .map(|s| match &s.hold {
            Some(Hold::Skipped(w)) => Outcome::Skipped(w.clone()),
            Some(Hold::Blocked(w)) => Outcome::Blocked(w.clone()),
            None if s.diff.is_empty() => Outcome::Unchanged,
            None => Outcome::Applied,
        })
        .collect();
    let applying: Vec<usize> = (0..d.specs.len())
        .filter(|i| d.specs[*i].applies())
        .collect();
    if applying.is_empty() {
        writeln!(out, "\nNothing to do.")?;
        return finish(d, &outcomes, out);
    }
    if consent == Consent::Ask && !prompt.literal_yes(out)? {
        writeln!(
            out,
            "  skipped — nothing was installed, written or granted."
        )?;
        bail!("code-nav setup --wizard not applied");
    }

    // Shared installs: once each; a failure fails every target needing it.
    writeln!(out, "\nInstalling shared tools:")?;
    let mut broken: BTreeMap<&'static str, String> = BTreeMap::new();
    for row in table::shared_installs(d) {
        if let Err(e) = ops.install_shared(&row, out) {
            writeln!(out, "  ✗ {} {}: {e:#}", row.name, row.version)?;
            broken.insert(row.name, format!("{e:#}"));
        }
    }

    let mut staged: BTreeMap<usize, (O::Staged, Vec<PathBuf>)> = BTreeMap::new();
    for &i in &applying {
        let s = &d.specs[i];
        if let Some((name, _)) = s
            .plan
            .install
            .iter()
            .find_map(|r| broken.get_key_value(r.name))
        {
            outcomes[i] = Outcome::InstallFailed(format!("shared install {name} failed (above)"));
            continue;
        }
        writeln!(out, "\n{}:", label(d, i))?;
        match ops.install(i, out) {
            Ok(v) => {
                staged.insert(i, v);
            }
            Err(e) => {
                writeln!(out, "  ✗ {e:#}")?;
                outcomes[i] = Outcome::InstallFailed(format!("{e:#}"));
            }
        }
    }

    if !lanes_approved(d, &staged, consent, prompt, out)? {
        writeln!(
            out,
            "  skipped — no exec lane, serena entry or manifest record was written for them."
        )?;
        for (i, (_, lanes)) in &staged {
            if !lanes.is_empty() {
                outcomes[*i] = Outcome::LanesDeclined;
            }
        }
        staged.retain(|_, (_, lanes)| lanes.is_empty());
    }

    for (i, (st, _)) in staged {
        writeln!(out, "\n{} — commit:", label(d, i))?;
        match ops.commit(i, st, out) {
            Ok(m) => {
                if let Err(e) = ops.smoke(i, &m, out) {
                    writeln!(out, "  ✗ {e:#}")?;
                    outcomes[i] = Outcome::SmokeFailed(format!("{e:#}"));
                }
            }
            Err(e) => {
                writeln!(out, "  ✗ {e:#}")?;
                outcomes[i] = Outcome::CommitFailed(format!("{e:#}"));
            }
        }
    }
    let restart_failed = restart_step(d, &outcomes, consent, restart, prompt, ops, out)?;
    finish(d, &outcomes, out)?;
    if restart_failed {
        bail!("code-nav setup --wizard: a restart failed (see above)");
    }
    Ok(())
}

/// One merged question for every new lane, each listed once with the agents
/// that receive it. `--yes` grants them, as single-agent `--yes` does.
fn lanes_approved<S>(
    d: &Decided,
    staged: &BTreeMap<usize, (S, Vec<PathBuf>)>,
    consent: Consent,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<bool> {
    let mut lanes: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for (i, (_, ls)) in staged {
        let agent = &d.agents[d.specs[*i].agent].name;
        for l in ls {
            match lanes.iter_mut().find(|(p, _)| p == l) {
                Some((_, a)) if !a.contains(agent) => a.push(agent.clone()),
                Some(_) => {}
                None => lanes.push((l.clone(), vec![agent.clone()])),
            }
        }
    }
    if lanes.is_empty() {
        return Ok(true);
    }
    table::print_lanes(&lanes, out)?;
    Ok(consent == Consent::Given || prompt.literal_yes(out)?)
}

/// Step 2. Returns whether any restart failed.
fn restart_step<O: Ops>(
    d: &Decided,
    outcomes: &[Outcome],
    consent: Consent,
    restart: bool,
    prompt: &mut dyn Prompt,
    ops: &mut O,
    out: &mut dyn Write,
) -> Result<bool> {
    let mut eligible: Vec<&str> = Vec::new();
    for (ai, a) in d.agents.iter().enumerate() {
        let mine: Vec<&Outcome> = d
            .specs
            .iter()
            .zip(outcomes)
            .filter(|(s, _)| s.agent == ai)
            .map(|(_, o)| o)
            .collect();
        // Checked first: an install or commit failure may already have
        // written grants (ast-grep) with nothing committed for the agent.
        if let Some(o) = mine.iter().find(|o| o.blocks_restart()) {
            writeln!(
                out,
                "\n  {} not restarted: a target {}; review it, then `mur agent restart {}`",
                a.name,
                match o {
                    Outcome::LanesDeclined => "had its exec lanes declined".to_owned(),
                    Outcome::InstallFailed(_) => "failed to install".to_owned(),
                    _ => "failed to commit".to_owned(),
                },
                a.name
            )?;
            continue;
        }
        if !mine.iter().any(|o| o.committed()) {
            continue;
        }
        if !ops.running(&a.name) {
            writeln!(
                out,
                "\n  {} is stopped; the change applies on its next start.",
                a.name
            )?;
            continue;
        }
        eligible.push(&a.name);
    }
    if eligible.is_empty() {
        return Ok(false);
    }
    let list = eligible.join(" ");
    let go = restart
        || (consent == Consent::Ask
            && prompt.confirm(&format!(
                "Permissions are granted. Restart {} now so they take effect?",
                eligible.join(", ")
            ))?);
    if !go {
        writeln!(out, "\nRestart to apply: mur agent restart {list}")?;
        return Ok(false);
    }
    let mut failed = false;
    for name in eligible {
        if !ops.running(name) {
            writeln!(
                out,
                "  - {name}: stopped meanwhile; applies on its next start"
            )?;
            continue;
        }
        match ops.restart(name) {
            Ok((true, _)) => writeln!(out, "  ✓ {name} restarted")?,
            Ok((false, detail)) => {
                failed = true;
                writeln!(
                    out,
                    "  ✗ {name}: {detail} — retry: mur agent restart {name}"
                )?;
            }
            Err(e) => {
                failed = true;
                writeln!(out, "  ✗ {name}: {e:#} — retry: mur agent restart {name}")?;
            }
        }
    }
    Ok(failed)
}

fn label(d: &Decided, i: usize) -> String {
    let s = &d.specs[i];
    let a = &d.agents[s.agent].name;
    match s.project {
        Some(p) => format!("{a} → {} ({})", d.projects[p].path.display(), s.entry()),
        None => format!("{a} (ast-grep only)"),
    }
}

/// One row per target; non-zero exit when any target failed.
fn finish(d: &Decided, outcomes: &[Outcome], out: &mut dyn Write) -> Result<()> {
    writeln!(out, "\nResult:")?;
    let mut failed = BTreeSet::new();
    for (i, o) in outcomes.iter().enumerate() {
        let text = match o {
            Outcome::Unchanged => "unchanged".to_owned(),
            Outcome::Applied => "applied".to_owned(),
            Outcome::Skipped(w) => format!("skipped ({w})"),
            Outcome::Blocked(w) => format!("not applied: {w}"),
            Outcome::InstallFailed(w) => format!("install failed: {w}"),
            Outcome::LanesDeclined => "exec lanes declined; nothing recorded".to_owned(),
            Outcome::CommitFailed(w) => format!("commit failed: {w}"),
            Outcome::SmokeFailed(_) => "applied; smoke launch failed (see above)".to_owned(),
        };
        let mark = if o.failed() { "✗" } else { "✓" };
        writeln!(out, "  {mark} {}: {text}", label(d, i))?;
        if o.failed() {
            failed.insert(i);
        }
    }
    if !failed.is_empty() {
        bail!(
            "code-nav setup --wizard: {} of {} target(s) failed",
            failed.len(),
            outcomes.len()
        );
    }
    Ok(())
}
