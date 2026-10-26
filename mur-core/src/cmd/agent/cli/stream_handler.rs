//! Stream-message handling, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Moved verbatim, except that an egress denial now adds its
//! notice line only once per (server, host).

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
            // Read lane: auto-approves read-only tools (`--no-auto-reads` off).
            // `read_file` is read-only by construction (a dedicated read
            // tool, sandbox-enforced); bash needs a provably read-only
            // command. Reads never ask, mirroring Claude Code — the gate
            // that used to hit every read_file bought zero safety: an
            // approved `bash cat` could read the same files anyway, so the
            // prompt was friction that trained blind approval.
            // #008: the session lane is STANDING, unattended authority, so it
            // is bounded by the same ceiling as a config grant —
            // `tier_may_be_granted`, capped at Write, which gate A already
            // consults (`mur-core/src/hitl/gate.rs:82`). Until now gate B had
            // no tier at all, so `/auto` (and, since #1228, the default
            // session) answered `rm -rf`, `curl`, `sudo` and `git push
            // --force` with nobody in the loop. The ceiling gates all three
            // auto lanes at once; none of them can reach past it.
            let tier = req.tier();
            let within_ceiling = mur_common::hitl::tier_may_be_granted(tier);
            let read_auto = within_ceiling
                && app.auto_reads
                && bash_class::is_readonly_call(&req.tool_name, Some(&req.tool_input));
            // A destination scope is its OWN proof and so is not bounded by
            // the tier ceiling: `tier_may_be_granted` is a check on a tier
            // guessed from a command head, while a scope is a parsed, one-hop,
            // read-only-at-both-ends command. Refusing to honour a grant the
            // operator was explicitly offered — which is what the ceiling did
            // to every `ssh … tail`, silently — is not a safety property, it
            // is the prompt storm that trains blind approval. A scope is only
            // ever minted for reads (`dest::classify_bash`); writes and second
            // hops return `None` and keep asking.
            let scoped_auto = dest::grant_for(&req.tool_name, Some(&req.tool_input), tier)
                .key()
                .is_some_and(|k| app.session_tool_allow.contains(&k));
            let auto = scoped_auto
                || (within_ceiling
                    && (app.auto_approve
                        || app.session_tool_allow.contains(&req.tool_name)
                        || read_auto));
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
            // A new gate always opens at the top of its own input, with the
            // narrowest answer (`Yes`, this call only) under the cursor.
            app.hitl_scroll = 0;
            app.hitl_selected = 0;
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
                Ok((reply, task_id)) => {
                    app.finish_agent_turn(reply, task_id);
                    // A chip the runtime attached (e.g. an MCP edit only a
                    // restart applies); `vet`ted like a `propose` call.
                    if let Some(p) = stream::attached_proposal(&task) {
                        proposal::offer(app, p);
                    }
                }
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
        StreamMsg::RestartDone(outcome) => proposal::finish_restart(app, outcome),
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
            } else if name == mur_common::proposal::PROPOSE_TOOL {
                // No step card. Same vet as the runtime's executor, so a
                // proposal the model is told was rejected is never shown.
                if let Ok(p) = mur_common::proposal::vet(&args) {
                    proposal::offer(app, p);
                }
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
        StreamMsg::StepTokens {
            step_id, tokens, ..
        } => app.update_step_tokens(&step_id, tokens),
        StreamMsg::EgressDenied(ev) => on_egress_denied(app, ev),
        StreamMsg::ReviewReq(req) => review::on_request(app, req),
        StreamMsg::ReviewFinished(outcome) => review::on_finished(app, outcome),
        StreamMsg::ReviewCancelFailed(e) => review::keys::on_cancel_failed(app, &e),
        StreamMsg::ManageDone(res) => {
            app.manage_busy = None;
            super::slash_cmds::apply_managed(app, res);
        }
    }
}

/// #1907: one notice line (with the paste-able command, so nothing is lost if
/// the chooser is pre-empted), and the 這次放行 / 永久放行 / 不用 chooser,
/// revealed when the turn ends so it never interrupts it.
fn on_egress_denied(app: &mut App, ev: mur_common::egress_event::EgressDenied) {
    use super::egress_denied as ed;
    let allow: Vec<String> = crate::cmd::agent::load_profile_for_edit(&app.agent)
        .ok()
        .and_then(|(_, p)| p.mcp_servers.into_iter().find(|e| e.name == ev.server))
        .and_then(|e| e.network)
        .map(|n| n.allow_hosts)
        .unwrap_or_default();
    let chips = ed::chips(&ev, &mut app.egress_chips, &allow);
    if chips.is_none() && app.egress_chips.dismissed.contains(&ev.host) {
        return;
    }
    if app.egress_chips.first_notice(&ev) {
        app.push_system(ed::text_fallback(&ev));
    }
    if let Some(c) = chips {
        app.pending_suggestions = c;
    }
}

// ── OS notifications ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::egress_event::{DenyReason, EgressDenied};

    /// No profile exists for this agent under a temp home, so the allow-list
    /// lookup yields nothing and only the chip/notice logic is exercised.
    const AGENT: &str = "egress-notice-test-agent";

    fn app_at(home: &tempfile::TempDir) -> App {
        let session = Session::create(home.path(), AGENT).unwrap();
        App::new(
            home.path().to_path_buf(),
            AGENT.into(),
            session,
            &super::super::theme::ANSI,
        )
    }

    fn denied(server: &str, host: &str) -> EgressDenied {
        EgressDenied {
            agent: AGENT.into(),
            server: server.into(),
            host: host.into(),
            port: Some(443),
            mode: "restricted".into(),
            reason: DenyReason::NotAllowed,
            at: "2026-10-15T00:00:00Z".into(),
            task_id: None,
        }
    }

    fn deliver(app: &mut App, ev: EgressDenied) {
        let (tx, _rx) = mpsc::channel(1);
        handle_stream(app, StreamMsg::EgressDenied(ev), &tx);
    }

    fn notices_for(app: &App, host: &str) -> usize {
        app.messages
            .iter()
            .filter(|m| m.role == Role::System && m.text.contains(host))
            .count()
    }

    #[test]
    fn repeated_browser_denials_leave_one_line_but_keep_chips() {
        let home = tempfile::tempdir().unwrap();
        let mut app = app_at(&home);
        for _ in 0..3 {
            app.pending_suggestions.clear();
            deliver(&mut app, denied("browser", "gator.volces.com"));
            assert!(
                !app.pending_suggestions.is_empty(),
                "every denial refreshes the chooser"
            );
        }
        assert_eq!(notices_for(&app, "gator.volces.com"), 1);
        deliver(&mut app, denied("browser", "cdn.example.com"));
        assert_eq!(notices_for(&app, "cdn.example.com"), 1);
    }

    #[test]
    fn repeated_non_browser_denials_leave_one_line_and_no_chips() {
        let home = tempfile::tempdir().unwrap();
        let mut app = app_at(&home);
        for _ in 0..3 {
            deliver(&mut app, denied("fetch", "api.example.com"));
        }
        assert_eq!(notices_for(&app, "api.example.com"), 1);
        assert!(app.pending_suggestions.is_empty());
    }

    #[test]
    fn dismissed_browser_host_stays_silent() {
        let home = tempfile::tempdir().unwrap();
        let mut app = app_at(&home);
        app.egress_chips.dismissed.insert("gator.volces.com".into());
        deliver(&mut app, denied("browser", "gator.volces.com"));
        assert_eq!(notices_for(&app, "gator.volces.com"), 0);
        assert!(app.pending_suggestions.is_empty());
    }
}
