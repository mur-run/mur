//! Cancel, quit and approval decisions, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;
use crate::cmd::fleet::review::murmur::bridge::HitlOrigin;

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
    if !review::keys::on_request_quit(app) {
        return;
    }
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

/// Act on the highlighted approval-menu row. This is the ONLY place a
/// session-wide grant is handed out: a grant now costs a deliberate move to a
/// labelled row plus Enter, so no single reflex keystroke can widen the
/// blast radius past the one call in front of the operator.
pub(super) fn commit_hitl_choice(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    let choice = super::ui::hitl_choices(app.hitl_origin)
        .get(app.hitl_selected)
        .copied()
        .unwrap_or(super::ui::HitlChoice::Once);
    match choice {
        super::ui::HitlChoice::Once => decide_hitl(app, tx, true),
        super::ui::HitlChoice::Tool => {
            // The grant is computed by the same function that WORDED the row,
            // so what gets stored is exactly what the operator was shown. It
            // used to be `req.tool_name` unconditionally, which is how a
            // picked grant could be silently worth nothing.
            if let Some(req) = &app.hitl {
                let tier = req.tier();
                let grant = super::dest::grant_for(&req.tool_name, Some(&req.tool_input), tier);
                match grant.key() {
                    Some(key) => {
                        app.session_tool_allow.insert(key.clone());
                        app.push_system(format!("won't ask again this session for {key}"));
                    }
                    // Saying so is the point: an operator who is told the
                    // grant did not take can pick `/auto` or keep answering,
                    // instead of pressing a row that does nothing forever.
                    None => app.push_system(
                        "approved this call — but it can't be remembered, so the next one still asks",
                    ),
                }
            }
            decide_hitl(app, tx, true);
        }
        super::ui::HitlChoice::All => {
            // This row only flips `auto_approve`, and the stream handler ANDs
            // that with the tier ceiling, so it can never silence a call
            // above `Write`. With auto already on (the session default) it
            // changes nothing at all; say so rather than leave the operator
            // to watch the same prompt come back (#1878).
            if app.auto_approve {
                app.push_system(
                    "auto-approve was already on — risky calls (network, destructive, privileged) still ask",
                );
            } else {
                app.auto_approve = true;
                app.push_system(
                    "auto-approve on for this session — risky calls (network, destructive, privileged) still ask",
                );
            }
            decide_hitl(app, tx, true);
        }
        super::ui::HitlChoice::Deny => {
            decide_hitl(app, tx, false);
            send_deny_reason(app, tx);
        }
    }
}

/// The "No" row promises `send "…" as the reason`, so keep the promise: what
/// the composer holds goes to the still-running turn as a steer, right behind
/// the denial. It used to stay in the composer, unsent, until a second Enter.
/// A review member's gate has nowhere to carry it, so it is left alone there.
fn send_deny_reason(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    if app.hitl_origin == HitlOrigin::Review {
        return;
    }
    let reason = app.input_text().trim().to_string();
    if reason.is_empty() || !app.streaming {
        return;
    }
    let Some(task_id) = app.current_task_id.clone() else {
        return;
    };
    app.clear_input();
    super::turn::steer_now(app, task_id, reason.clone(), &reason, tx);
}

/// Retire an approval request that has outlived the gate's timeout.
///
/// The runtime denies the call on its own deadline and says nothing about it:
/// `StreamMsg::Expired` only arrives when the operator answers too LATE, so a
/// gate nobody touched stayed in `app.hitl` forever. The status line went on
/// advertising "tool approval needed (auto-deny in 0s)" for a request that was
/// already dead — the one surface still claiming a decision was wanted.
///
/// Reads the request's `deadline` — the one the panel counts down to, set from
/// the runtime's own `timeout_ms` — so the display, this sweep and the runtime
/// cannot disagree. A profile that shortens `hitl.timeout_secs` now shortens
/// the countdown too.
///
/// Returns whether anything was retired.
pub(super) fn expire_stale_hitl(app: &mut App) -> bool {
    let Some(req) = &app.hitl else { return false };
    if StdInstant::now() < req.deadline {
        return false;
    }
    let tool = req.tool_name.clone();
    let step = req.step_id.clone();
    app.hitl = None;
    review::hitl::settle_cleared(app);
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
    review::hitl::promote(app);
}

pub(super) fn decide_hitl_with_note(
    app: &mut App,
    tx: &mpsc::Sender<StreamMsg>,
    allow: bool,
    auto: bool,
) {
    // P3b-§10: a member's gate is answered by the review worker, never here.
    if app.hitl_origin == HitlOrigin::Review {
        return review::hitl::decide(app, allow);
    }
    if let Some(req) = app.hitl.take() {
        app.hitl_resolved_at = Some(std::time::Instant::now());
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
        // /auto, the read lane and session grants, which the audit trail must not
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
            // An approved call is simply a call that ran: its card says so,
            // with the command. The separate `✔ approved …` row it replaces
            // made one call read as two events (spec 2026-09-29 §4).
            (true, false) => {
                app.saw_hitl_this_turn = true;
            }
            // A denial keeps its own row, because nothing else records it.
            // It names the COMMAND, not the tool category — a bare `denied
            // \`bash\`` could not tell two denials apart after the fact.
            (false, _) => app.push_denied(denial_line(&req.tool_name, &req.tool_input, app.width)),
        }
    }
}

/// `edit_file ~/murui-demo/hello.md — denied`: the tool, what it would have
/// touched, and the verdict. The model-written intent is left off — it says
/// why the agent wanted the call, which no longer matters once it is refused.
pub(super) fn denial_line(tool: &str, input: &serde_json::Value, width: u16) -> String {
    let s = super::call_summary::approval_summary(
        tool,
        input,
        width,
        super::call_summary::ApprovalFrame::Transcript,
    );
    let (cmd, _intent) = super::call_summary::split_intent(&s);
    let cmd = cmd.replacen(" · ", " ", 1);
    format!("{} — denied", cmd.trim_end())
}

/// HITL prompt owns the decision keys — but Ctrl+C/Ctrl+D must stay
/// live so the user is never trapped by a stale/unanswerable modal,
/// and any other key keeps going to the composer so typed text isn't
/// silently swallowed while the modal is up.
///
/// Decision LETTERS only count while the composer is empty. Once the
/// operator has typed anything they are writing a message, not
/// answering the gate, and `y`/`a`/`n` are ordinary characters —
/// before this guard, sending "modal" while a gate was open ate the
/// `a`, approved the call, AND added the tool to
/// `session_tool_allow` for the whole session. Esc and the Ctrl
/// combos are exempt: they cannot collide with typed text, and Esc
/// must always remain the escape hatch.
pub(super) fn handle_hitl_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    ctrl: bool,
    tx: &mpsc::Sender<StreamMsg>,
) {
    // Option A: the "No" row is where a reason is written. With the cursor
    // there, every character types — a reason that starts with `n`, `y` or a
    // digit ("name it hello.txt") used to deny on its first letter, because
    // the composer was still empty when that letter landed.
    // A review member's "No" carries no reason, so its row stays a plain
    // answer and its digit still denies at once.
    let deny_row = deny_row(app).filter(|_| app.hitl_origin != HitlOrigin::Review);
    let on_deny = Some(app.hitl_selected) == deny_row;
    let shortcuts = app.input_text().is_empty() && !on_deny;
    match key.code {
        KeyCode::Char('d') if ctrl => request_quit(app, tx),
        KeyCode::Char('c') if ctrl => decide_hitl(app, tx, false),
        // The composer guard above makes y/a/A/n type instead of
        // decide whenever the composer holds text, so the way out
        // of that state has to keep working. It did not: the
        // catch-all below forwarded Ctrl+U into the textarea, which
        // deletes one character, leaving no live key that approves
        // and only the 5-minute auto-deny to end the gate (#939).
        KeyCode::Char('u') if ctrl => app.clear_input(),
        // The docked panel folds a long call instead of scrolling
        // it (a third scroll owner would collide with ↑/↓ and
        // PgUp), so PgUp/PgDn keep scrolling the transcript — the
        // agent's reasons for the call — and Ctrl+O shows the call
        // in full. Both used to be swallowed or typed while a gate
        // was open.
        KeyCode::PageUp => app.scroll_back = app.scroll_back.saturating_add(app.scroll_page.max(1)),
        KeyCode::PageDown => {
            app.scroll_back = app.scroll_back.saturating_sub(app.scroll_page.max(1))
        }
        KeyCode::Char('o') if ctrl => scrollback_dump(app),
        KeyCode::Char('y') | KeyCode::Char('Y') if shortcuts => decide_hitl(app, tx, true),
        // The menu replaces the old two-press `a`/`A` dance. Arrows
        // and Enter cannot collide with typed text, so they stay
        // live even while the composer holds a message — a denial
        // can carry that message along. Only the digit shortcuts
        // step aside and type when the composer is non-empty.
        KeyCode::Up => {
            app.hitl_selected = app.hitl_selected.saturating_sub(1);
        }
        KeyCode::Down => {
            app.hitl_selected =
                (app.hitl_selected + 1).min(ui::hitl_choices(app.hitl_origin).len() - 1);
        }
        // Tab, and the "No" row's own digit, move the cursor there WITHOUT
        // deciding, so the reason can be typed next. Enter then denies with
        // it (or without one, if nothing was typed).
        KeyCode::Tab => {
            if let Some(i) = deny_row {
                app.hitl_selected = i;
            }
        }
        KeyCode::Char(c @ '1'..='4') if shortcuts => {
            let i = (c as usize) - ('1' as usize);
            if Some(i) == deny_row {
                app.hitl_selected = i;
            } else if i < ui::hitl_choices(app.hitl_origin).len() {
                app.hitl_selected = i;
                commit_hitl_choice(app, tx);
            }
        }
        KeyCode::Enter => commit_hitl_choice(app, tx),
        KeyCode::Char('n') | KeyCode::Char('N') if shortcuts => decide_hitl(app, tx, false),
        KeyCode::Esc => decide_hitl(app, tx, false),
        _ => {
            app.input.input(key);
        }
    }
}

/// Index of the "No" row in the open gate's menu.
fn deny_row(app: &App) -> Option<usize> {
    ui::hitl_choices(app.hitl_origin)
        .iter()
        .position(|c| *c == ui::HitlChoice::Deny)
}
