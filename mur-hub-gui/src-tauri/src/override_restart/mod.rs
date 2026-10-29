//! The Hub's startup-override restart (Hub spec §Command, §Launchers, §Quit).
//! The orchestrator lives in `mur_core`; this module supplies the Hub's job
//! table, launcher, native confirmation and the Tauri commands.
//!
//! `run_override` / `run_normal` are the commands' bodies with no Tauri
//! types, so the tests drive them with a fake launcher and confirmation.

pub mod checks;
pub mod commands;
pub mod dialog;
pub mod jobs;
pub mod launcher;
pub mod quit;

use std::path::{Path, PathBuf};

use chrono::Utc;
use mur_common::agent::OverrideFlags;
use mur_common::permissions::{AuditEvent, GrantStore};
use mur_common::startup_override::IssuedBy;
use mur_core::cmd::agent::{
    Launcher, Mode, NOTHING_CHANGED, OverrideOutcome, OverrideRequest, Prepared, RestartNote,
    outcome_text, override_restart, prepare, restart_normally, step_text,
};
use serde::Serialize;
use tracing::{info, warn};

use dialog::HubConfirmation;
use jobs::{Busy, JobEntry, JobState, OverrideJobs};

/// What `restart_agent_with_overrides` returns to the webview: the outcome
/// and the sentence the CLI would print for it.
#[derive(Debug, Serialize)]
pub struct OverrideReply {
    pub outcome: OverrideOutcome,
    pub message: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NormalReply {
    pub ok: bool,
    pub message: String,
}

/// The reply to a declined (or accepted-while-quitting) dialog.
pub fn cancelled_text() -> String {
    format!("Cancelled; {NOTHING_CHANGED}")
}

/// One job's fixed inputs. Owned, so it moves onto the blocking thread.
pub(crate) struct Ctx {
    pub home: PathBuf,
    pub name: String,
    pub window: String,
    pub jobs: OverrideJobs,
}

/// Check 4 plus its audit line: register the job or refuse it.
pub(crate) fn admit(ctx: &Ctx, initial: JobState) -> Result<JobEntry, String> {
    ctx.jobs
        .admit(&ctx.name, &ctx.window, initial)
        .map_err(|busy| {
            audit(
                &ctx.home,
                &ctx.name,
                &AuditEvent::StartupOverrideRefused {
                    ts: Utc::now(),
                    reason: busy.audit_reason().to_owned(),
                    window: Some(ctx.window.clone()),
                },
            );
            match busy {
                Busy::DialogOpen => "an override confirmation is already open".to_owned(),
                Busy::InProgress => {
                    format!("an override restart of {} is already in progress", ctx.name)
                }
            }
        })
}

/// Checks 5–7 (in `prepare`), the confirmation, then the orchestrator.
/// `entry` is held until return, so the job's slot is freed on every exit,
/// including a panic.
pub(crate) fn run_override(
    ctx: Ctx,
    entry: JobEntry,
    flags: OverrideFlags,
    confirm: &mut dyn FnMut(&Prepared) -> Option<HubConfirmation>,
    launcher: &mut dyn FnMut(&Prepared) -> Box<dyn Launcher>,
) -> Result<OverrideReply, String> {
    let _entry = entry;
    let prepared = prepare(&OverrideRequest {
        home: &ctx.home,
        name: &ctx.name,
        mode: Mode::Restart,
        flags,
        by: IssuedBy::Hub,
    })
    .map_err(|s| step_text(&s))?;

    let confirmation = match confirm(&prepared) {
        Some(c) if !ctx.jobs.is_quitting() && c.flags() == flags => c,
        _ => {
            drop(prepared);
            audit(
                &ctx.home,
                &ctx.name,
                &AuditEvent::StartupOverrideDeclined {
                    ts: Utc::now(),
                    flags,
                    window: Some(ctx.window.clone()),
                },
            );
            return Err(cancelled_text());
        }
    };
    ctx.jobs.set_state(&ctx.name, JobState::Preparing);

    let mut l = launcher(&prepared);
    let name = ctx.name.clone();
    let outcome = override_restart(
        prepared,
        confirmation.issuer(),
        Box::new(ctx.jobs.gate(&ctx.name)),
        l.as_mut(),
        &mut |n| log_note(&name, n),
    );
    Ok(OverrideReply {
        message: outcome_text(&ctx.name, &outcome),
        outcome,
    })
}

/// "Restart normally": the orphan sweep, then the ordinary restart.
pub(crate) fn run_normal(
    ctx: Ctx,
    entry: JobEntry,
    launcher: &mut dyn FnMut(&Prepared) -> Box<dyn Launcher>,
) -> Result<NormalReply, String> {
    let _entry = entry;
    let prepared = prepare(&OverrideRequest {
        home: &ctx.home,
        name: &ctx.name,
        mode: Mode::Restart,
        flags: OverrideFlags::default(),
        by: IssuedBy::Hub,
    })
    .map_err(|s| step_text(&s))?;
    let mut l = launcher(&prepared);
    let name = ctx.name.clone();
    Ok(
        match restart_normally(prepared, Some(ctx.window.clone()), l.as_mut(), &mut |n| {
            log_note(&name, n)
        }) {
            Ok(r) => NormalReply {
                ok: r.ok,
                message: r.detail,
            },
            Err(outcome) => NormalReply {
                ok: false,
                message: outcome_text(&ctx.name, &outcome),
            },
        },
    )
}

fn log_note(name: &str, note: RestartNote) {
    match note {
        RestartNote::Info(s) => info!(agent = %name, "{s}"),
        RestartNote::Failure(s) => warn!(agent = %name, "{s}"),
    }
}

/// Audit lines go straight to the agent's `permissions/audit.jsonl`. A
/// failed append is logged, never an error: nothing depends on it.
fn audit(home: &Path, name: &str, event: &AuditEvent) {
    if let Err(e) = GrantStore::new(home.join("agents").join(name)).append_audit(event) {
        warn!(agent = %name, "could not write the override audit line: {e}");
    }
}

#[cfg(all(test, unix))]
#[path = "run_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "guard_tests.rs"]
mod guard_tests;
