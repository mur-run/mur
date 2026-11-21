//! `mur fleet resume` over a damaged channel log (#2081 item 10).
//!
//! `signed_pause` folds the `run_paused` events it can read. A line that no
//! longer parses is skipped by the loader, and a line that was cut out is
//! simply not there, so if that line was the tightest pause the fold settles
//! on a looser one without a word. The log's own integrity check — the one
//! the HITL gate reads through — is what notices: resume takes its events
//! from that read and treats its verdict as "the signed pauses may be
//! incomplete".

use std::path::Path;

use anyhow::{Result, bail};

use super::super::checkpoint::Checkpoint;
use super::super::pause_events::RunBounds;
use super::{
    ResumeFrom, SignedPause, fold_pauses, require_authenticated, resume_from, signed_pause,
};

/// Under `MUR_CHANNEL_REQUIRE_SIG` a resume over a damaged log is refused
/// (#2081 item 10): a line that no longer parses, or was cut out, may be the
/// tightest `run_paused`, and what is left cannot say it was not.
pub(super) fn require_intact(
    name: &str,
    channel_id: &str,
    damage: Option<&str>,
    require_sig: bool,
) -> Result<()> {
    match damage {
        Some(why) if require_sig => bail!(
            "MUR_CHANNEL_REQUIRE_SIG is set and channel {channel_id} cannot vouch for the original limits of the paused run of fleet '{name}' ({why}): a run_paused with tighter limits may be missing; refusing to resume — inspect it with `mur channel verify {channel_id}`, or end the run with `mur fleet stop {name}`"
        ),
        _ => Ok(()),
    }
}

/// [`resume_from`] over a damaged log (#2081 item 10). The pauses still
/// readable are folded with the checkpoint's copy, so the checkpoint can
/// tighten the limits and never widen them: an honest run wrote its last
/// `run_paused` and its checkpoint with the same bounds, which recovers a
/// lost tightest pause. Nothing vouches for the result, so it is reported as
/// unauthenticated.
pub(super) fn resume_from_damaged(
    cp: &Checkpoint,
    signed: Option<SignedPause>,
    unattended_now: bool,
) -> Result<ResumeFrom> {
    let checkpoint_copy = SignedPause {
        spent_usd: cp.spent_usd,
        active_elapsed_ms: cp.active_elapsed_ms,
        iteration: cp.iteration,
        bounds: Some(RunBounds {
            deadline_ms: cp.deadline_ms,
            stuck_ms: cp.stuck_ms,
            cost_usd: cp.cost_usd,
        }),
    };
    let folded = fold_pauses(signed.into_iter().chain([checkpoint_copy]));
    let mut from = resume_from(cp, folded, unattended_now)?;
    from.bounds_authenticated = false;
    Ok(from)
}

/// What `mur fleet resume` re-enters with, and — when the channel's log is
/// damaged — why it could not vouch for the limits. One read of the channel,
/// through the integrity check, decides both.
pub(super) fn checked_resume(
    mur_home: &Path,
    fleet: &mur_common::fleet::Fleet,
    cp: &Checkpoint,
    unattended_now: bool,
    require_sig: bool,
) -> Result<(ResumeFrom, Option<String>)> {
    let channel_id = &fleet.channel_id;
    let checked = crate::hitl::gate::read_checked(mur_home, channel_id).map_err(|e| {
        anyhow::anyhow!("cannot read channel {channel_id} to verify the paused run's limits: {e}")
    })?;
    require_intact(
        &fleet.name,
        channel_id,
        checked.damage.as_deref(),
        require_sig,
    )?;
    let signed = signed_pause(mur_home, channel_id, &checked.events, &cp.run_id)?;
    let from = match checked.damage {
        Some(_) => resume_from_damaged(cp, signed, unattended_now)?,
        None => resume_from(cp, signed, unattended_now)?,
    };
    require_authenticated(&fleet.name, from.bounds_authenticated, require_sig)?;
    Ok((from, checked.damage))
}
