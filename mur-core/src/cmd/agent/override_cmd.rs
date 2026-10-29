//! `mur agent start|restart <name> --<override>`: argument checks, the
//! confirmation step, the orchestrator, and the outcome as a message and an
//! exit code. Applied (on time or late) exits 0; everything else fails.

use std::io::IsTerminal as _;

use anyhow::{Result, bail};
use mur_common::agent::OverrideFlags;
use mur_common::agent_name::validate_agent_name;
use mur_common::startup_override::IssuedBy;

use super::override_restart::{
    AlwaysYes, Cleanup, Confirmation, Issuer, Mode, OverrideOutcome, OverrideRequest, Step,
    Terminal, audit, confirm, override_restart, prepare, run::FILE_REMAINS,
};
use super::resolve_mur_home;
use super::restart::{CliLauncher, Launcher, StartLauncher, print_note};

pub fn cmd_start_override(name: &str, flags: OverrideFlags, flag_confirm: bool) -> Result<()> {
    run(name, Mode::Start, flags, flag_confirm)
}

/// With an override, `restart` takes exactly one name and no selectors;
/// anything else is refused before anything is written.
pub fn cmd_restart_override(
    names: &[String],
    all: bool,
    stale: bool,
    dry_run: bool,
    flags: OverrideFlags,
    flag_confirm: bool,
) -> Result<()> {
    let name = single_target(names, all, stale, dry_run)?;
    run(name, Mode::Restart, flags, flag_confirm)
}

pub(super) fn single_target(
    names: &[String],
    all: bool,
    stale: bool,
    dry_run: bool,
) -> Result<&str> {
    let refuse = |what: &str| -> Result<&str> {
        bail!("an override restart takes exactly one agent name, not {what}; nothing was changed")
    };
    if all {
        return refuse("--all");
    }
    if stale {
        return refuse("--stale");
    }
    if dry_run {
        return refuse("--dry-run");
    }
    match names {
        [one] => Ok(one),
        [] => refuse("none"),
        _ => refuse("several"),
    }
}

fn run(name: &str, mode: Mode, flags: OverrideFlags, flag_confirm: bool) -> Result<()> {
    validate_agent_name(name).map_err(|e| anyhow::anyhow!("{e}; nothing was changed"))?;
    let home = resolve_mur_home()?;
    let req = OverrideRequest {
        home: &home,
        name,
        mode,
        flags,
        by: IssuedBy::Cli,
    };
    let prepared = prepare(&req).map_err(|s| anyhow::anyhow!(step_text(&s)))?;

    let stdin = std::io::stdin();
    let is_tty = stdin.is_terminal() && std::io::stderr().is_terminal();
    let confirmed_by = match confirm(
        name,
        flags,
        flag_confirm,
        Terminal {
            is_tty,
            input: &mut stdin.lock(),
            output: &mut std::io::stderr(),
        },
    ) {
        Confirmation::Confirmed(by) => by,
        Confirmation::Declined => {
            audit::declined(&home, name, flags);
            bail!("cancelled; nothing was changed");
        }
        Confirmation::NeedsConfirmFlag => bail!(
            "an override needs confirmation: run it in a terminal, or pass --confirm; \
             nothing was changed"
        ),
    };

    let has_service = prepared.has_service();
    let mut launcher: Box<dyn Launcher> = match mode {
        Mode::Restart => Box::new(CliLauncher::new(name, &home, has_service)),
        Mode::Start => Box::new(StartLauncher::new(name, &home, has_service)),
    };
    let outcome = override_restart(
        prepared,
        Issuer::Cli { confirmed_by },
        Box::new(AlwaysYes),
        launcher.as_mut(),
        &mut print_note,
    );
    report(name, &outcome)
}

/// The outcome as the CLI says it (Hub spec §Failure paths).
pub(super) fn report(name: &str, outcome: &OverrideOutcome) -> Result<()> {
    debug_assert_eq!(
        outcome.is_success(),
        matches!(outcome, OverrideOutcome::Applied { .. })
    );
    match outcome {
        OverrideOutcome::Applied { flags, detail, .. } => {
            println!(
                "✓ '{name}' is running with the override: {}",
                flags.cli_names().join(" ")
            );
            if let Some(d) = detail {
                eprintln!("  (an earlier step reported: {d})");
            }
            Ok(())
        }
        OverrideOutcome::Rejected { reason, detail, .. } => {
            let mut msg = format!(
                "'{name}' restarted, but rejected the override: {reason}. It is running with \
                 normal permissions."
            );
            if let Some(d) = detail {
                msg.push_str(&format!(" (an earlier step reported: {d})"));
            }
            bail!(msg)
        }
        OverrideOutcome::Failed {
            step,
            cleanup,
            detail,
        } => bail!(failed_text(name, step, cleanup.as_ref(), detail)),
        OverrideOutcome::Abandoned { reason } => bail!("override abandoned: {reason}"),
    }
}

fn failed_text(name: &str, step: &Step, cleanup: Option<&Cleanup>, detail: &str) -> String {
    let suffix = match cleanup {
        None => return detail.to_owned(),
        Some(Cleanup::Withdrawn) => match step {
            Step::NoAck => format!(
                "the override was withdrawn; '{name}' may still come up, with normal permissions"
            ),
            _ => "the override was withdrawn and nothing is relaxed".to_owned(),
        },
        Some(Cleanup::ConsumedUnacknowledged) => format!(
            "the override was consumed by a boot that has not acknowledged it; it may still come \
             up relaxed. Run `mur agent status {name}`"
        ),
        Some(Cleanup::WithdrawFailed { error }) => format!("{FILE_REMAINS} ({error})"),
    };
    format!("{detail}; {suffix}")
}

fn step_text(step: &Step) -> String {
    match step {
        Step::Refusal { reason } => reason.clone(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
#[path = "override_cmd_tests.rs"]
mod tests;
