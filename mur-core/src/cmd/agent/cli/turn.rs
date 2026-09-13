//! Turn and shell-turn lifecycle, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;

pub(super) async fn submit(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    app.clear_suggestion_ghost();
    let mut trimmed = app.input_text().trim().to_string();
    // Allow an image-only send (caption optional) when a screenshot is staged.
    if trimmed.is_empty() && app.pending_image.is_none() {
        return;
    }
    app.history_record(&trimmed);

    if let Some(cmd) = parse_slash(&trimmed) {
        // Skills are surfaced in the completion menu as slash commands
        // (`/brainstorming`) but are not built-ins, so parse_slash reports them
        // as Unknown. Route a matched skill to the agent as an invocation (the
        // runtime's trigger matcher picks it up) rather than erroring.
        if matches!(cmd, SlashCmd::Unknown(_))
            && let Some((skill, args)) = complete::matched_skill(&trimmed, &app.skills)
        {
            let extra = if args.is_empty() {
                String::new()
            } else {
                format!(" {args}")
            };
            trimmed = format!("Use the `{skill}` skill.{extra}");
            // fall through into the normal send path below
        } else {
            app.clear_input();
            handle_slash(app, cmd, tx).await;
            // One refresh site, not four. `/model`, `/secret`, `/remember` and
            // `/forget` each change one of these lists, and `handle_slash` has
            // early returns in most arms, so a per-arm refresh would rot the
            // first time an arm gains a return. Slash commands are typed by a
            // human; three small file reads per command is not a cost.
            app.menu_ctx = complete::MenuContext::load(&app.home, &app.agent);
            return;
        }
    }
    // `!command` — run locally (like Claude Code's bang escape). Allowed while
    // a turn is generating: it never touches the agent connection.
    if let Some(cmd) = trimmed.strip_prefix('!').map(str::trim)
        && !cmd.is_empty()
    {
        app.clear_input();
        // D7: one foreground job. Refuse before spawning — and never assign
        // over a live handle, which would drop its sender and silently
        // cancel the command already running.
        if app.shell.is_running() {
            app.push_system("a `!command` is already running — Ctrl-C to stop it");
            return;
        }
        let (child, pid) = match shell::spawn(cmd).await {
            Ok(v) => v,
            Err(e) => {
                // Nothing to cancel and nothing to stream: one finished card.
                app.begin_shell(cmd);
                let end = shell::ShellEnd::SpawnFailed(e.to_string());
                let output = app.finish_shell(&end);
                finish_shell_turn(app, cmd, &end, output, tx);
                return;
            }
        };
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let Some(gen_id) = app.shell.begin(pid, cancel_tx) else {
            // Unreachable given the guard above; refuse rather than leak.
            shell::signal_group(pid, shell::SIGKILL_NUM);
            return;
        };
        // D2: the card exists from the keypress, so `!sleep 45` is visibly
        // running rather than silent for 45 seconds. No "running …" system
        // line: the card carries its own footer, and two indicators for one
        // command is one too many.
        app.begin_shell(cmd);
        let (cmd, t) = (cmd.to_string(), tx.clone());
        tokio::spawn(async move {
            let end = shell::run(child, pid, gen_id, t.clone(), cancel_rx).await;
            let _ = t.send(StreamMsg::ShellDone { gen_id, cmd, end }).await;
        });
        return;
    }
    // While a turn is generating: steer it if we have a live task id,
    // otherwise fall back to the old reject message.
    if app.streaming {
        // `turn/steer` carries a string, so an image cannot ride a steer: the
        // text would go, the image would stay staged, and the agent would
        // truthfully answer "no image" — or, for an image-only send, the
        // runtime would reject the empty steer. Hold the whole message
        // instead. ponytail: queueing it for the next turn needs a hook at
        // every turn-end site in app.rs; do that if "press Enter again" grates.
        if app.pending_image.is_some() {
            app.push_system(
                "📎 an image can't steer a running turn — wait for it to finish (or Ctrl+C), then press Enter again",
            );
            return;
        }
        if let Some(task_id) = app.current_task_id.clone() {
            app.clear_input();
            steer_now(app, task_id, trimmed.clone(), &trimmed, tx);
        } else {
            app.push_system("still generating — press Ctrl+C to cancel first");
        }
        return;
    }
    // Session budget cap: refuse a NEW turn once estimated spend hits the cap.
    // (An in-flight turn is handled by the streaming branch above; this only
    // gates starting a fresh one.) Fails open when the model has no pricing, so
    // it never blocks turns whose cost we can't estimate. Input is left intact
    // so the user can copy what they composed.
    if app.over_budget() {
        let cap = app.budget_usd.unwrap_or(0.0);
        let spent = app.session_cost().unwrap_or(0.0);
        app.push_system(format!(
            "↯ session budget reached — spent ~${spent:.2} of ${cap:.2}. Restart `mur agent cli` to reset."
        ));
        return;
    }

    app.last_sent = Some(trimmed.clone());
    // A fresh send supersedes any expired-approval retry offer (#8).
    app.expired_retry = None;
    app.clear_input();
    start_turn(app, trimmed, tx);
}

/// Start a fresh `message/send` turn: record + persist the user text, build
/// the params, and spawn the streaming worker. The params are kept on `app`
/// so a dial that dies before the runtime starts the turn can be replayed
/// once — by then the user's message is already persisted to the channel and
/// must not be dropped silently (#714).
pub(super) fn start_turn(app: &mut App, trimmed: String, tx: &mpsc::Sender<StreamMsg>) {
    let task_id = app.begin_user_turn(&trimmed);
    // The working directory is NOT prose here — it rides as `context.cwd`
    // in `build_params`, every turn.
    let params = build_params(
        &trimmed,
        &task_id,
        app.context_task_id.as_deref(),
        app.pending_image
            .as_ref()
            .map(|(m, b)| (m.as_str(), b.as_str())),
        app.cwd.as_deref(),
    );
    app.pending_image = None;
    app.inflight_params = Some(params.clone());
    spawn_stream(
        app.home.clone(),
        app.agent.clone(),
        params,
        task_id,
        tx.clone(),
    );
}

/// End the running `!cmd` from any exit — Ctrl-C, quit, `/clear`, a channel
/// switch — and close its card on the spot (D8, D11).
///
/// `hard` is quit: signal the group synchronously, because nothing will be
/// left to run a grace timer. Nothing is routed to the agent either way: the
/// user stopped this on purpose (D4).
pub(super) fn stop_shell(app: &mut App, hard: bool) {
    if !shell::cancel(&mut app.shell, hard) {
        return; // nothing was running
    }
    // Stamp `[cancelled]`, clear `streaming` (which also stops the footer
    // rendering as live), and persist. The late `ShellDone` will be dropped
    // by the generation check, so this is the only chance to do it.
    let _ = app.finish_shell(&shell::ShellEnd::Cancelled);
}

/// Route a finished `!cmd`'s output: start a turn, steer the live one, or
/// say why it went nowhere. The card is already finalised by the caller.
pub(super) fn finish_shell_turn(
    app: &mut App,
    cmd: &str,
    end: &shell::ShellEnd,
    output: String,
    tx: &mpsc::Sender<StreamMsg>,
) {
    let block = shell_block(cmd, &shell::cap_tail(&output, shell::SHELL_MAX_BYTES));
    match route_shell_output(
        !end.reaches_agent(),
        app.streaming,
        app.current_task_id.as_deref(),
        app.over_budget(),
    ) {
        ShellRoute::Start => start_shell_turn(app, block, tx),
        ShellRoute::Steer(task_id) => {
            let label = format!("$ {cmd} output");
            steer_now(app, task_id, block, &label, tx);
        }
        ShellRoute::Skip(why) => app.push_system(why),
    }
}

/// Start a turn whose transcript entry is the Shell card already opened by
/// `begin_shell` and closed by `finish_shell`: no User bubble, no second
/// channel event. A staged image is left staged — it belongs to the user's
/// next typed message.
pub(super) fn start_shell_turn(app: &mut App, block: String, tx: &mpsc::Sender<StreamMsg>) {
    let task_id = app.begin_turn();
    let params = build_params(
        &block,
        &task_id,
        app.context_task_id.as_deref(),
        None,
        app.cwd.as_deref(),
    );
    app.inflight_params = Some(params.clone());
    spawn_stream(
        app.home.clone(),
        app.agent.clone(),
        params,
        task_id,
        tx.clone(),
    );
}

/// Inject `msg` into the live turn `task_id`. `label` is what the transcript
/// shows after "↗ steering:" — the typed text for a message, a short tag for
/// a shell block that would otherwise fill the screen twice.
pub(super) fn steer_now(
    app: &mut App,
    task_id: String,
    msg: String,
    label: &str,
    tx: &mpsc::Sender<StreamMsg>,
) {
    let (h, a) = (app.home.clone(), app.agent.clone());
    let t = tx.clone();
    app.push_system(format!("↗ steering: {label}"));
    tokio::spawn(async move {
        if let Err(e) = stream::steer_turn(h, a, task_id.clone(), msg.clone()).await {
            let err = format!("{e:#}");
            let out = match recover::classify_steer_failure(&err) {
                // The runtime restarted (tasks live in memory only):
                // the steered task is gone. Drop the dead binding and
                // replay the text as a fresh turn on the same channel so
                // it is not lost (#713).
                recover::SteerFailure::TaskGone => StreamMsg::TurnLost {
                    task_id,
                    note: "agent restarted — continuing in this conversation".to_string(),
                    resend: Some(msg),
                },
                recover::SteerFailure::Other => StreamMsg::Note(format!("steer failed: {err}")),
            };
            let _ = t.send(out).await;
        }
    });
}
