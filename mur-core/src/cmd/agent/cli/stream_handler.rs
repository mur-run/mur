//! Stream-message handling, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;

pub(super) fn handle_stream(app: &mut App, msg: StreamMsg, tx: &mpsc::Sender<StreamMsg>) {
    // Drop events from a turn that is no longer current (cancelled, cleared, or
    // already finished) so a still-running worker can't splice its tokens/reply
    // into a later turn. `Note` carries no task id and is always shown.
    if let Some(tid) = msg.task_id()
        && app.current_task_id.as_deref() != Some(tid)
    {
        return;
    }
    // Any runtime-originated event means the turn started on the peer; from
    // here a failed dial is never replayed (the agent may have done
    // side-effectful work already).
    if matches!(
        msg,
        StreamMsg::Delta { .. }
            | StreamMsg::Hitl { .. }
            | StreamMsg::StepStarted { .. }
            | StreamMsg::StepCompleted { .. }
    ) {
        app.turn_produced_output = true;
    }
    match msg {
        StreamMsg::Delta { text, thinking, .. } => app.append_delta(&text, thinking),
        StreamMsg::Hitl { req, task_id } => {
            // One slot on screen. A batched response delivers N gates at once;
            // the rest wait their turn (`promote_queued_hitl`), each keeping its
            // own `hitl_id` and its own countdown.
            if app.hitl.is_some() {
                app.hitl_queue.push_back((task_id, req));
                return;
            }
            // NB: `saw_hitl_this_turn` is set in `decide`, on approval — not
            // here. See its doc comment (#940).
            // Flag the card so it renders the inline approval row. Whether
            // that row is actually VISIBLE is recomputed every frame by the
            // renderer (`ui::inline_row_visible`) rather than cached here: a
            // card that is live now can be flushed into scrollback while the
            // gate is still open, and a cached "inline is showing" would keep
            // the modal suppressed over rows that can no longer repaint.
            if let Some(sid) = req.step_id.clone() {
                app.mark_card_awaiting(&sid);
            }
            // Session auto-approval: `/auto`/`--auto` covers every tool; the
            // modal's [a] key covers a single tool name.
            // Read lane: `--auto-reads` auto-approves read-only tools.
            // `read_file` is read-only by construction (a dedicated read
            // tool, sandbox-enforced); bash needs a provably read-only
            // command. Reads never ask, mirroring Claude Code — the gate
            // that used to hit every read_file bought zero safety: an
            // approved `bash cat` could read the same files anyway, so the
            // prompt was friction that trained blind approval.
            let read_auto = app.auto_reads
                && bash_class::is_readonly_call(&req.tool_name, Some(&req.tool_input));
            let auto =
                app.auto_approve || app.session_tool_allow.contains(&req.tool_name) || read_auto;
            if !app.focused && !auto {
                notify_unfocused(
                    &app.agent,
                    &format!("Tool approval needed: {}", req.tool_name),
                );
            }
            if read_auto && let Some(ref sid) = req.step_id {
                app.mark_card_auto_approved(sid);
            }
            app.hitl = Some(req);
            // A new gate always opens at the top of its own input.
            app.hitl_scroll = 0;
            if auto {
                decide_hitl_with_note(app, tx, true, true);
            }
        }
        StreamMsg::Done { task, .. } => {
            if !app.focused {
                notify_unfocused(&app.agent, "Turn finished");
            }
            if let Some(u) = task.get("usage") {
                app.apply_usage(u);
            }
            app.maybe_step_hint();
            match stream::task_outcome(&task) {
                Ok((reply, task_id)) => app.finish_agent_turn(reply, task_id),
                Err(cause) => app.fail_turn(&cause),
            }
            app.reveal_suggestions();
        }
        StreamMsg::Err { error, .. } => {
            // A dial that died before the runtime produced anything for this
            // turn means the user's message never became a task — yet it is
            // already persisted in the channel. Replay it once; on a second
            // failure say loudly (and persist) that it was not delivered,
            // never drop it silently (#714).
            if recover::should_retry_send(app.turn_produced_output, app.send_retried)
                && let Some(params) = app.inflight_params.clone()
            {
                app.send_retried = true;
                app.push_warn(format!("turn failed to start ({error}) — retrying once…"));
                retry_send(app, params, tx);
                return;
            }
            if !app.focused {
                notify_unfocused(&app.agent, "Turn failed");
            }
            let failed_to_start = !app.turn_produced_output;
            app.fail_turn(&error);
            if failed_to_start {
                app.mark_undelivered();
            }
        }
        StreamMsg::Note(text) => app.push_system(text),
        StreamMsg::Expired { tool, retry } => {
            // The gate auto-denied `tool` at timeout before this approval
            // landed (#8). Stash the user's last message so [Ctrl+R] can
            // refill the composer for a one-key re-run, and hint that path
            // instead of the old dead-end "re-run the request" note.
            app.expired_retry = retry;
            let hint = if app.expired_retry.is_some() {
                " — press Ctrl+R to re-run the request"
            } else {
                " — re-run the request"
            };
            app.push_system(format!(
                "approval for `{tool}` arrived too late — the call was already \
                 auto-denied at timeout{hint}"
            ));
        }
        StreamMsg::TurnLost { note, resend, .. } => {
            app.drop_dead_turn();
            app.push_system(note);
            if let Some(text) = resend {
                start_turn(app, text, tx);
            }
        }
        StreamMsg::ShellOutput { gen_id, chunk } => {
            // D8: a retired generation is a command the user already walked
            // away from; its output must not land in whatever conversation
            // is open now.
            if app.shell.accepts(gen_id) {
                app.append_shell_output(&chunk);
            }
        }
        StreamMsg::ShellDone { gen_id, cmd, end } => {
            // Unconditional: the child is gone, so its pid stops being ours
            // to kill. This is the resource question, and it must be answered
            // even for a generation the UI has retired — otherwise a quit
            // would keep signalling a dead group (§3.5).
            app.shell.done(gen_id);
            // Conditional: a retired generation has already had its card
            // finalised by `stop_shell` (D11), so there is nothing to draw
            // and nothing to route.
            if !app.shell.accepts(gen_id) {
                return;
            }
            let output = app.finish_shell(&end);
            finish_shell_turn(app, &cmd, &end, output, tx);
        }
        StreamMsg::ShellCardDone { gen_id, end } => {
            app.shell.done(gen_id);
            if app.shell.accepts(gen_id) {
                let _ = app.finish_shell(&end);
            }
        }
        StreamMsg::StepStarted {
            step_id,
            name,
            args,
            ..
        } => {
            if name == suggest::SUGGEST_REPLIES_NAME {
                // No step card: stash replies and reveal at turn end.
                app.pending_suggestions = suggest::parse_suggestions(&args);
            } else {
                app.saw_step_this_turn = true;
                // A delegated fleet run is a long, otherwise-opaque step:
                // auto-arm the member rail + milestone follow so the user
                // watches it happen instead of staring at a spinner.
                if name == fleet_rail::FLEET_RUN_TOOL
                    && let Some(fleet) = args.get("fleet").and_then(|v| v.as_str())
                {
                    let fleet = fleet.to_string();
                    app.arm_auto_fleet(&step_id, &fleet, StdInstant::now());
                }
                app.push_step_started(step_id, name, args);
            }
        }
        StreamMsg::StepCompleted {
            step_id,
            ok,
            output,
            truncated,
            full_len,
            error,
            duration_ms,
            denied,
            running,
            ..
        } => {
            app.update_step_completed(
                &step_id,
                ok,
                output,
                truncated,
                full_len,
                error,
                duration_ms,
                denied,
                running,
            );
            app.finish_auto_fleet(&step_id, ok, duration_ms);
        }
    }
}

// ── OS notifications ─────────────────────────────────────────────────────────
