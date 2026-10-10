//! P3b-§10: a review member's tool approval through the shared modal. The UI
//! only decides; the worker owns `hitl_respond` (the gate belongs to the
//! member, not to the attached agent `respond_hitl` would dial).
//!
//! One slot on screen (`app.hitl`). `app.hitl_origin` says who answers it;
//! for `Review` the decision goes to `app.review_gate.reply` and nowhere
//! else. The transport asks one call at a time, but an own-turn gate may
//! already hold the slot, so a review gate can wait in `review_gate.queue`.

use std::collections::VecDeque;
use std::sync::mpsc::SyncSender;

use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::call_summary::{ApprovalFrame, approval_summary};
use crate::cmd::agent::cli::stream::HitlRequest;
use crate::cmd::fleet::review::murmur::bridge::HitlOrigin;

/// The review half of the approval slot.
#[derive(Default)]
pub struct ReviewGate {
    /// Where the decision for the review gate in `app.hitl` goes.
    pub reply: Option<SyncSender<bool>>,
    /// The member whose turn raised it, for the modal title and receipt.
    pub member: Option<String>,
    /// Review gates waiting for the slot.
    pub queue: VecDeque<(String, HitlRequest, SyncSender<bool>)>,
}

/// The worker's `Hitl`. Always asks: `auto_approve`, the read lane and session
/// grants are the attached agent's, never a member's.
pub fn on_gate(app: &mut App, member: String, req: HitlRequest, reply: SyncSender<bool>) {
    app.review_gate.queue.push_back((member, req, reply));
    promote(app);
}

/// Put the next waiting review gate in the slot if it is free. Returns
/// whether one was shown.
pub fn promote(app: &mut App) -> bool {
    if app.hitl.is_some() {
        return false;
    }
    let Some((member, req, reply)) = app.review_gate.queue.pop_front() else {
        return false;
    };
    if !app.focused {
        crate::cmd::agent::cli::notify::notify_unfocused(
            &app.agent,
            &format!("Review tool approval needed: {member} · {}", req.tool_name),
        );
    }
    app.hitl = Some(req);
    app.hitl_origin = HitlOrigin::Review;
    app.review_gate.reply = Some(reply);
    app.review_gate.member = Some(member);
    // Same as a fresh own gate: top of the input, narrowest answer selected.
    app.hitl_scroll = 0;
    app.hitl_selected = 0;
    true
}

/// Answer the review gate in the slot. A send error means the worker already
/// ended; its `Finished` follows.
pub fn decide(app: &mut App, allow: bool) {
    let Some(req) = app.hitl.take() else { return };
    app.hitl_resolved_at = Some(std::time::Instant::now());
    let member = app.review_gate.member.take().unwrap_or_default();
    if let Some(reply) = app.review_gate.reply.take() {
        let _ = reply.send(allow);
    }
    app.hitl_origin = HitlOrigin::Own;
    let s = approval_summary(
        &req.tool_name,
        &req.tool_input,
        app.width,
        ApprovalFrame::Transcript,
    );
    if allow {
        app.push_success(format!("{member}: approved {s}"));
    } else {
        app.push_warn(format!("{member}: denied {s}"));
    }
}

/// The slot was emptied by something other than a decision (timeout, a new
/// conversation): a review gate there is denied, so the worker never waits
/// on a modal nobody can see. A no-op for an own gate.
pub fn settle_cleared(app: &mut App) {
    if let Some(reply) = app.review_gate.reply.take() {
        let _ = reply.send(false);
    }
    app.review_gate.member = None;
    app.hitl_origin = HitlOrigin::Own;
}

/// The review ended or MURMUR is closing: every gate it raised is denied.
/// Dropping a queued reply reads as deny on the worker side.
pub fn drop_all(app: &mut App) {
    app.review_gate.queue.clear();
    if app.hitl_origin == HitlOrigin::Review {
        app.hitl = None;
        settle_cleared(app);
    }
}
