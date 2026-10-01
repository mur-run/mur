//! Workflow `delegate_to` steps through the write-grant gate (#1607).
//!
//! A `delegate_to` step only reaches its member when the run has a channel
//! (`dag.rs`: the delegation branch needs both a target and a channel id);
//! without one it falls through to intent mode and dials nobody. So the gate
//! runs only for channel runs, and a channel-less run with delegate steps gets
//! a warning instead of an error — there is nothing to block.
//!
//! `mur workflow run` has no `--cwd`: the target is the repo root of the
//! directory it was invoked from, marked inferred, so the gate always asks.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use mur_common::skill::manifest::Procedure;

use crate::executor::delegation::cwd::{routing_note, routing_target};
use crate::executor::delegation::grant::{
    GrantContext, block_message, ensure_write_grants, targets_for,
};
use crate::hitl::gate::GatePolicy;

/// The canonical, de-duplicated members a procedure delegates to, in step
/// order, one entry per profile however it is spelled.
pub fn delegate_members(mur_home: &Path, procedure: &Procedure) -> Vec<String> {
    let mut seen = HashSet::new();
    procedure
        .steps
        .iter()
        .filter_map(|s| s.delegate_to.as_deref())
        .map(|t| crate::a2a_dial::canonicalize_agent_name(mur_home, t))
        // Case-insensitive: on a case-insensitive filesystem canonicalize
        // returns `Coder` unchanged (its exact-match probe hits `coder/`), so
        // two spellings of one profile would otherwise ask twice.
        .filter(|m| seen.insert(m.to_ascii_lowercase()))
        .collect()
}

/// `procedure` with the routing note appended to every `delegate_to` step's
/// prompt (`intent`, else the description), so the member is sent to the
/// directory the gate checked. Labels (`description`) are untouched; non-
/// delegate steps are untouched.
pub fn with_routing(procedure: &Procedure, note: &str) -> Procedure {
    let mut out = procedure.clone();
    for s in out.steps.iter_mut().filter(|s| s.delegate_to.is_some()) {
        let base = s.intent.take().unwrap_or_else(|| s.description.clone());
        s.intent = Some(format!("{base}{note}"));
    }
    out
}

/// The warning for a channel-less run that has `delegate_to` steps. `None`
/// when there are none. Without a channel those steps fall through to intent
/// mode: they print and report success, and no member is ever dialled.
pub fn no_channel_warning(members: &[String]) -> Option<String> {
    if members.is_empty() {
        return None;
    }
    Some(format!(
        "warning: this workflow delegates to {} but runs without a channel, so those members \
         will not be called — their steps only print. To delegate for real, re-run with \
         --channel-new (or --channel <id>).",
        members.join(", ")
    ))
}

/// Gate every member `procedure` delegates to on `work_dir`'s routing target.
/// `Ok(None)` = dispatch may proceed (including "nothing delegates");
/// `Ok(Some(msg))` = blocked, `msg` names each member and its fix. Always
/// inferred: `mur workflow run` has no `--cwd`, the directory is a guess.
pub async fn gate_workflow(
    mur_home: &Path,
    procedure: &Procedure,
    channel_id: &str,
    work_dir: &Path,
    policy: GatePolicy,
) -> Result<Option<String>> {
    let members = delegate_members(mur_home, procedure);
    if members.is_empty() {
        return Ok(None);
    }
    let run_id = format!("grant-{}", uuid::Uuid::now_v7());
    let ctx = GrantContext {
        mur_home,
        channel_id,
        run_id: &run_id,
        policy,
        job_count: procedure
            .steps
            .iter()
            .filter(|s| s.delegate_to.is_some())
            .count(),
    };
    let target = routing_target(work_dir);
    let targets = targets_for(members.iter().map(String::as_str), &target, true);
    let results = ensure_write_grants(&ctx, &targets).await?;
    Ok(block_message(&results, channel_id))
}

/// What `mur workflow run` hands the DAG. `policy` is built the way the DAG
/// builds it for its own risk-tiered steps, so the grant prompt answers to the
/// same `--yes` / TTY rules as every other gate in the run. On a channel run: gate every
/// delegate member (bail if any is blocked), then append the routing note so
/// members are sent where they were checked. Without a channel: warn if any
/// step delegates (it will only print), and pass the procedure through.
pub async fn prepare_procedure<'p>(
    mur_home: &Path,
    procedure: &'p Procedure,
    channel_id: Option<&str>,
    work_dir: &Path,
    policy: GatePolicy,
) -> Result<Cow<'p, Procedure>> {
    let Some(cid) = channel_id else {
        if let Some(w) = no_channel_warning(&delegate_members(mur_home, procedure)) {
            eprintln!("{w}");
        }
        return Ok(Cow::Borrowed(procedure));
    };
    if !procedure.steps.iter().any(|s| s.delegate_to.is_some()) {
        return Ok(Cow::Borrowed(procedure));
    }
    if let Some(msg) = gate_workflow(mur_home, procedure, cid, work_dir, policy).await? {
        bail!("{msg}");
    }
    Ok(Cow::Owned(with_routing(
        procedure,
        &routing_note(work_dir, true),
    )))
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
