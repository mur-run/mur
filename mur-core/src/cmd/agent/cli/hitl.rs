//! Cancel, quit and approval decisions, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;

/// Cancel the in-flight turn (if any) on a separate connection and mark the
/// streaming bubble done locally. After this, `current_task_id` is `None`, so
/// the orphaned worker's late events are dropped by `handle_stream`.
pub(super) fn cancel_in_flight(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    if !app.streaming {
        return;
    }
    if let Some(task_id) = app.current_task_id.clone() {
        let (h, a, t) = (app.home.clone(), app.agent.clone(), tx.clone());
        tokio::spawn(async move {
            if let Err(e) = cancel_task(h, a, task_id).await {
                let _ = t
                    .send(StreamMsg::Note(format!("cancel failed: {e:#}")))
                    .await;
            }
        });
    }
    app.finish_partial();
}

/// Cancel any in-flight turn, then request TUI shutdown. Cancelling first lets
/// the runtime close the stream so the (detached) worker unblocks promptly and
/// stops doing abandoned server-side work.
pub(super) fn request_quit(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    cancel_in_flight(app, tx);
    // Hard: the event loop is about to stop, so nothing is left to run the
    // escalation timer, and dropping the task would only `kill_on_drop` the
    // direct shell and leave its group. Covers a slot still `Cancelling`
    // from a Ctrl-C moments ago, which is the case that orphaned (§3.5).
    stop_shell(app, true);
    app.should_quit = true;
}

pub(super) fn handle_ctrl_c(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    // D3: a running `!cmd` is what Ctrl-C ends — it is the thing the user
    // just launched and is watching. Any agent turn keeps running and still
    // has Esc-Esc. The state is retired here, so a second press falls
    // through to the behaviour below, unchanged.
    if app.shell.is_running() {
        stop_shell(app, false);
        return;
    }
    if app.streaming {
        cancel_in_flight(app, tx);
        app.push_system("cancelled");
    } else if app.input_text().trim().is_empty() {
        // Empty + idle: require a second Ctrl+C within a short window to quit,
        // matching Esc's arm-then-confirm so a stray keypress can't kill the
        // session. The arm state is disarmed by any other key (see event loop).
        if app
            .last_ctrl_c_at
            .is_some_and(|t| t.elapsed() < ESC_DOUBLE_WINDOW)
        {
            app.last_ctrl_c_at = None;
            app.ctrl_c_hint = false;
            app.should_quit = true;
        } else {
            app.last_ctrl_c_at = Some(std::time::Instant::now());
            app.ctrl_c_hint = true;
        }
    } else {
        app.clear_input();
    }
}

/// Answer the open HITL prompt, surfacing a dial failure (so a lost decision
/// isn't reported to the user as success).
pub(super) fn decide_hitl(app: &mut App, tx: &mpsc::Sender<StreamMsg>, allow: bool) {
    decide_hitl_with_note(app, tx, allow, false);
}

/// Retire an approval request that has outlived the gate's timeout.
///
/// The runtime denies the call on its own deadline and says nothing about it:
/// `StreamMsg::Expired` only arrives when the operator answers too LATE, so a
/// gate nobody touched stayed in `app.hitl` forever. The status line went on
/// advertising "tool approval needed (auto-deny in 0s)" for a request that was
/// already dead — the one surface still claiming a decision was wanted.
///
/// Uses the same `DEFAULT_TIMEOUT` the status line counts down to, so the
/// display and the state cannot disagree. (A profile that shortens
/// `hitl.timeout_secs` still expires earlier on the runtime side than the CLI
/// shows; the request carries no timeout for the CLI to read.)
///
/// Returns whether anything was retired.
pub(super) fn expire_stale_hitl(app: &mut App) -> bool {
    let Some(req) = &app.hitl else { return false };
    if req.created_at.elapsed() < crate::hitl::gate::DEFAULT_TIMEOUT {
        return false;
    }
    let tool = req.tool_name.clone();
    let step = req.step_id.clone();
    app.hitl = None;
    app.hitl_grant_confirm = None;
    // Same swallow window as a normal decision: a key pressed just as the gate
    // died must not land in the composer as text.
    app.hitl_resolved_at = Some(StdInstant::now());
    if let Some(sid) = step {
        app.clear_card_awaiting(&sid);
    }
    app.push_system_sev(
        format!("approval for `{tool}` timed out — auto-denied"),
        app::Severity::Warn,
    );
    app.needs_full_redraw = true;
    true
}

/// The slot emptied (decision, timeout, or auto-approval): show the next
/// queued gate, if any. Runs after every event or stream message, so a gate
/// answered by keypress promotes its successor on the same tick.
pub(super) fn promote_queued_hitl(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    if app.hitl.is_none()
        && let Some((task_id, req)) = app.hitl_queue.pop_front()
    {
        handle_stream(app, StreamMsg::Hitl { task_id, req }, tx);
    }
}

pub(super) fn decide_hitl_with_note(
    app: &mut App,
    tx: &mpsc::Sender<StreamMsg>,
    allow: bool,
    auto: bool,
) {
    if let Some(req) = app.hitl.take() {
        app.hitl_resolved_at = Some(std::time::Instant::now());
        app.hitl_grant_confirm = None;
        if let Some(sid) = &req.step_id {
            app.clear_card_awaiting(sid);
        }
        let (h, a) = (app.home.clone(), app.agent.clone());
        let (id, tool) = (req.hitl_id.clone(), req.tool_name.clone());
        let task_id = app.current_task_id.clone();
        // Capture the user's last message before the spawn so an expired
        // approval (#8) can offer a one-key re-run of the stranded request.
        let retry = app.last_sent.clone();
        let t = tx.clone();
        // A keypress is the only branch a human actually answered; `auto` covers
        // /auto, --auto-reads and session grants, which the audit trail must not
        // report as a person sitting there.
        let surface = if auto { "auto" } else { "cli" };
        tokio::spawn(async move {
            if let Err(e) = respond_hitl(h, a, id, allow, surface).await {
                let msg = format!("{e:#}");
                let out = match recover::classify_hitl_failure(&msg) {
                    // "approval expired" (new runtimes) / "task not found"
                    // (older runtimes) on this method both mean the same
                    // thing: the gate timed out and auto-denied before the
                    // decision landed. The turn itself is still alive.
                    recover::HitlFailure::Expired => StreamMsg::Expired {
                        tool: tool.clone(),
                        retry: retry.clone(),
                    },
                    // The runtime is gone: the whole in-memory task died with
                    // it, so the stale binding must be dropped too or every
                    // subsequent input steers a dead task (#713).
                    recover::HitlFailure::AgentGone if task_id.is_some() => StreamMsg::TurnLost {
                        task_id: task_id.unwrap_or_default(),
                        note: format!(
                            "failed to deliver decision for `{tool}`: {msg} — \
                                 your next message will start a fresh turn"
                        ),
                        resend: None,
                    },
                    _ => StreamMsg::Note(format!("failed to deliver decision for `{tool}`: {msg}")),
                };
                let _ = t.send(out).await;
            }
        });
        match (allow, auto) {
            // Auto-approval shows as a dim tag on the card itself, not as its
            // own transcript row. The row it replaces was emitted for every
            // single tool call and said nothing the card couldn't: it doubled
            // the height of the scrollback for zero extra information.
            (true, true) => {
                app.saw_hitl_this_turn = true;
                if let Some(sid) = &req.step_id {
                    app.mark_card_auto_approved(sid);
                }
            }
            (true, false) => {
                app.saw_hitl_this_turn = true;
                app.push_success(format!("approved `{}`", req.tool_name))
            }
            (false, _) => app.push_warn(format!("denied `{}`", req.tool_name)),
        }
    }
}
