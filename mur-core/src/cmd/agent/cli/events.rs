//! The event loop and key dispatch, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;

pub(super) async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    mut panel_rx: mpsc::Receiver<mur_common::panel::HubFrame>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<StreamMsg>(stream::STREAM_CHANNEL_CAP);
    let mut events = EventStream::new();
    let mut spinner = tokio::time::interval(Duration::from_millis(SPINNER_MS));
    let mut last_size = terminal.backend().size()?;
    // Inline-viewport height: fixed for the terminal's current size (see
    // `viewport_h_for`). Tracks the height the live terminal actually has
    // (run() creates the terminal with this same value).
    let mut viewport_h = viewport_h_for(last_size.height);

    loop {
        // Terminal size changed (font zoom, window resize): ratatui's
        // `autoresize` (inside `draw`) re-anchors an Inline viewport with
        // `append_lines`, leaking up to viewport-height blank rows into
        // scrollback on EVERY size change. Detect the change ourselves
        // (ioctl, no cursor query) and rebuild the terminal at the current
        // anchor instead, so `autoresize` never fires.
        if app.render_mode == RenderMode::Inline {
            let size = terminal.backend().size()?;
            if size != last_size {
                // Rebuilding queries the cursor position by reading the
                // terminal's stdin response — drop the EventStream first so
                // that read doesn't hang (see the viewport comment in `run`).
                drop(events);
                // Fail-open: if the rebuild can't read the cursor position,
                // keep the old terminal — ratatui's autoresize will leak a
                // stale viewport copy (cosmetic), which beats exiting.
                if let Ok(h) = rebuild_after_resize(terminal, app) {
                    viewport_h = h;
                }
                events = EventStream::new();
                last_size = terminal.backend().size()?;
                // Cached replies were rendered for the old width; tables in
                // them chose their columns from it. The rebuild replays the
                // transcript from index 0, so re-render before that paint.
                app.width = last_size.width.max(1);
                app.rerender_markdown();
            }
        }
        // One ioctl per pass keeps every width-sensitive row (composer hint,
        // status line, tool-card arg hints) honest in both render modes —
        // `last_size` above is only refreshed on the Inline path.
        app.width = terminal.backend().size()?.width.max(1);
        app.sync_input_block();
        // /clear or channel switch: the on-screen transcript no longer
        // matches the conversation — wipe screen + scrollback and re-anchor
        // so the fresh state (welcome or replayed channel) renders clean.
        //
        // The height is derived HERE, from the state this wipe is about to
        // render, and committed only once `purge_and_reanchor` has actually
        // installed it. Both halves matter:
        //
        // - `purge_and_reanchor` can fail (the cursor-position query), and its
        //   error is deliberately swallowed to keep the session alive. Setting
        //   `viewport_h` before calling it therefore recorded a height the
        //   terminal never took, and every later `flush_finished` / `render` /
        //   `insert_before` measured the band against geometry that was not on
        //   screen. Leaving it uncommitted also makes the next pass see the
        //   mismatch again and retry, which is the right response to a
        //   transient failure — and it cannot spin, because a terminal that
        //   never answers the cursor query could not have started murmur
        //   (`run`'s own `with_options` is a hard error).
        // - `/clear` empties the transcript, so it changes the height AND
        //   requests the wipe in one step. Deriving from the stale local ran
        //   the purge at the outgoing height, and the block above then found
        //   the mismatch and purged a second time — two full-screen clears and
        //   two replays for one `/clear`.
        if app.render_mode == RenderMode::Inline && std::mem::take(&mut app.wants_screen_wipe) {
            drop(events);
            let want_h = viewport_h_for(last_size.height);
            let banner = app.welcome_applies().then(|| app.welcome_banner());
            if purge_and_reanchor(terminal, want_h, banner.as_deref()).is_ok() {
                viewport_h = want_h;
            }
            events = EventStream::new();
        }
        // A slash command (`/login`'s escalating repair) asked for the real
        // terminal. Handled here, not in `handle_slash`, because only the
        // loop owns `terminal` and `events`. Guarded on Inline like the three
        // blocks above it: everything below assumes an Inline viewport, and
        // re-anchoring one while the alt-screen is up would anchor it against
        // the wrong surface. Leaving the request in place is correct — it is
        // taken on the pass after the overlay closes, not dropped.
        if app.render_mode == RenderMode::Inline
            && let Some(req) = app.pending_handover.take()
        {
            let want_h = prepare_handover(app, &req.label, last_size.height);
            // The EventStream owns stdin: the child must have it to itself,
            // and the re-anchor below reads the terminal's cursor-position
            // reply from it — `Terminal::with_options(Inline(..))` issues a
            // cursor query and needs crossterm's reader lock, which the
            // EventStream's background thread holds. So this drop is required
            // here, not merely tidy.
            //
            // Behaviour change from moving it above the flush and the draw:
            // keystrokes typed during those two calls used to die with the
            // dropped EventStream and are now left in the terminal's own
            // input buffer for the child to read. That is the improvement —
            // the window is short, but anything typed in it was the user
            // answering the login prompt, and swallowing it was never right.
            drop(events);
            // Re-anchor the REAL terminal, not just this local: the draw
            // below and `Suspended::begin`'s clear would otherwise be working
            // from two different geometries. Fail-open on Err, like the resize
            // path — keeping the old height costs a cosmetic mis-anchor,
            // bailing costs the session.
            if want_h != viewport_h && handover::reanchor(terminal, want_h).is_ok() {
                viewport_h = want_h;
            }
            // Paint the "handing over" line before we suspend — otherwise it
            // only appears after the child exits, alongside the result.
            ui::flush_finished(terminal, app, viewport_h)?;
            terminal.draw(|f| ui::render(f, app))?;
            let outcome = handover::run(terminal, viewport_h, &req);
            events = EventStream::new();
            match outcome {
                Ok(s) if s.success() => {
                    app.push_system(format!(
                        "{}: logged in ✓ — no restart needed, the gateway re-reads per request",
                        req.label
                    ));
                }
                Ok(s) => app.push_error(format!("{}: login exited with {s}", req.label)),
                Err(e) => app.push_error(format!("{}: handover failed: {e:#}", req.label)),
            }
            app.needs_full_redraw = true;
        }
        // Same shape as the handover above, and for the same reason: the
        // EventStream owns stdin and the child (here, rpassword's tty read)
        // must have it to itself.
        if app.render_mode == RenderMode::Inline
            && let Some(key) = app.pending_secret_prompt.take()
        {
            let want_h = prepare_handover(app, &format!("/secret {key}"), last_size.height);
            drop(events);
            if want_h != viewport_h && handover::reanchor(terminal, want_h).is_ok() {
                viewport_h = want_h;
            }
            ui::flush_finished(terminal, app, viewport_h)?;
            terminal.draw(|f| ui::render(f, app))?;
            let read = handover::read_hidden(
                terminal,
                viewport_h,
                &format!("Enter value for {key} (input hidden, Enter alone cancels): "),
            );
            events = EventStream::new();
            match read {
                Ok(value) => secret_cmd::after_hidden_input(app, key, value).await,
                Err(e) => app.push_error(format!("{key}: hidden read failed: {e:#}")),
            }
            app.needs_full_redraw = true;
        }
        if let Some(key) = app.pending_secret_delete.take() {
            secret_cmd::after_delete(app, key).await;
        }
        arm_input_debounce(app, StdInstant::now());
        app.refresh_monitor_counts(StdInstant::now());
        // Flush the live band's overflow into native scrollback BEFORE the
        // draw, so the band always paints a screenful of the newest content
        // and the composer stays glued to the screen bottom. No-op in
        // Fullscreen mode and while the band still fits.
        ui::flush_finished(terminal, app, viewport_h)?;

        // Keep the terminal surface in sync with the render mode BEFORE the
        // draw: an overlay open/close this iteration may have toggled
        // `render_mode`, and the draw below must land on the matching
        // surface (small inline viewport vs. full-frame alt-screen).
        sync_surface(app)?;
        // Retire an expired gate BEFORE drawing. Running it after the draw
        // cleared the state but left the stale frame on screen: the loop wakes
        // at the deadline, paints the still-open gate, retires it, then sleeps
        // with nothing left to wake it — so the status line went on asking for
        // a decision on a request that had already been denied, which is the
        // very thing retiring it was meant to stop.
        if expire_stale_hitl(app) {
            promote_queued_hitl(app, &tx);
        }
        if app.needs_full_redraw {
            terminal.clear()?;
            app.needs_full_redraw = false;
        }
        terminal.draw(|f| ui::render(f, app))?;
        if app.should_quit {
            return Ok(());
        }
        let input_due = app
            .panel_input_deadline
            .map(TokioInstant::from_std)
            .unwrap_or_else(|| TokioInstant::from_std(StdInstant::now()));
        let input_armed = app.panel_input_deadline.is_some();
        // A followed channel is polled on its own deadline (not a per-iteration
        // `sleep`, which every keypress would reset — during a busy turn the
        // tail would never fire).
        let follow_armed = app.follow.is_some();
        let follow_at = app
            .follow
            .as_ref()
            .map(|f| TokioInstant::from_std(f.next_poll))
            .unwrap_or_else(|| TokioInstant::from_std(StdInstant::now()));
        // Fleet rail: cheap when nothing moved (two metadata calls), and only
        // forces a redraw when the folded view actually changed.
        if let Some(rail) = app.fleet.as_mut()
            && StdInstant::now() >= rail.next_poll()
            && rail.poll(&app.home, StdInstant::now())
        {
            app.needs_full_redraw = true;
        }
        // The rail needs its own wake source, exactly like `follow`: without
        // this arm, an idle loop (no keypresses, no streaming, transcript
        // non-empty so `blink_at` is disarmed) never wakes on its own, the
        // poll above never gets a turn, and the rail goes stale forever on a
        // terminal the user is just reading. The arm body is empty on
        // purpose — waking the loop is the whole job; the poll above runs at
        // the top of the next iteration.
        let rail_armed = app.fleet.is_some();
        let rail_at = app
            .fleet
            .as_ref()
            .map(|r| TokioInstant::from_std(r.next_poll()))
            .unwrap_or_else(|| TokioInstant::from_std(StdInstant::now()));
        // Wake exactly at the expiry, or an idle loop (nothing streaming, no
        // blink, no rail) never notices its own countdown reaching zero. The
        // sweep itself runs before the draw at the top of the loop.
        let hitl_armed = app.hitl.is_some();
        let hitl_at = app
            .hitl
            .as_ref()
            .map(|r| TokioInstant::from_std(r.created_at + crate::hitl::gate::DEFAULT_TIMEOUT))
            .unwrap_or_else(|| TokioInstant::from_std(StdInstant::now()));
        tokio::select! {
            maybe = events.next() => match maybe {
                Some(Ok(ev)) => {
                    handle_event(app, ev, &tx).await;
                    promote_queued_hitl(app, &tx);
                }
                Some(Err(_)) | None => return Ok(()),
            },
            Some(msg) = rx.recv() => {
                handle_stream(app, msg, &tx);
                promote_queued_hitl(app, &tx);
            }
            // Never closes: PanelHandle in `app` holds a keepalive sender,
            // so this arm can't spin on a dead channel.
            Some(f) = panel_rx.recv() => match f {
                mur_common::panel::HubFrame::Insert { text } => app.set_input(&text),
            },
            _ = spinner.tick(), if app.streaming || app.shell.is_running() => app.tick_spinner(),
            _ = tokio::time::sleep_until(follow_at), if follow_armed => {
                app.poll_follow(StdInstant::now());
            }
            // Wake at the rail's next-poll deadline; the poll itself already
            // ran at the top of THIS iteration and gates on the same
            // deadline, so this arm's only job is to schedule the NEXT
            // wake-up. No state change needed here.
            _ = tokio::time::sleep_until(rail_at), if rail_armed => {}
            _ = tokio::time::sleep_until(hitl_at), if hitl_armed => {}
            _ = tokio::time::sleep_until(input_due), if input_armed => {
                if let Some(raw) = take_due_input(app, StdInstant::now())
                    && let Some(p) = &app.panel
                {
                    p.send(mur_common::panel::PanelFrame::InputChanged {
                        text: mur_common::panel::input_snapshot(&raw),
                    });
                }
            }
        }
    }
}

pub(super) async fn handle_event(app: &mut App, ev: Event, tx: &mpsc::Sender<StreamMsg>) {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            // The transcript overlay (Ctrl+O) is a full-screen view drawn over
            // everything else, including the HITL modal. It stays in raw mode
            // and routes every key through the pure `overlay_key_action`
            // dispatch instead of dropping to a blocking stdin read, so
            // Ctrl+D/Esc are never swallowed and no key ever leaks into the
            // composer once it closes.
            if app.overlay_open {
                match overlay_key_action(key.code, key.modifiers) {
                    OverlayKeyAction::Close => {
                        app.overlay_open = false;
                        app.overlay_text = None;
                        leave_fullscreen(app);
                    }
                    OverlayKeyAction::CloseAndQuit => {
                        app.overlay_open = false;
                        app.overlay_text = None;
                        leave_fullscreen(app);
                        request_quit(app, tx);
                    }
                    OverlayKeyAction::Ignore => {}
                }
                return;
            }
            // HITL prompt owns the decision keys — but Ctrl+C/Ctrl+D must stay
            // live so the user is never trapped by a stale/unanswerable modal,
            // and any other key keeps going to the composer so typed text isn't
            // silently swallowed while the modal is up.
            //
            // Decision LETTERS only count while the composer is empty. Once the
            // operator has typed anything they are writing a message, not
            // answering the gate, and `y`/`a`/`n` are ordinary characters —
            // before this guard, sending "modal" while a gate was open ate the
            // `a`, approved the call, AND added the tool to
            // `session_tool_allow` for the whole session. Esc and the Ctrl
            // combos are exempt: they cannot collide with typed text, and Esc
            // must always remain the escape hatch.
            if app.hitl.is_some() {
                let composer_empty = app.input_text().is_empty();
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
                    // Paging the modal body, which now wraps and keeps the
                    // whole tool input rather than cutting it (#939).
                    KeyCode::PageUp => {
                        app.hitl_scroll = app
                            .hitl_scroll
                            .saturating_sub(ui::hitl_scroll_step(app.hitl_page))
                    }
                    KeyCode::PageDown => {
                        app.hitl_scroll = app
                            .hitl_scroll
                            .saturating_add(ui::hitl_scroll_step(app.hitl_page))
                    }
                    KeyCode::Char('y') | KeyCode::Char('Y') if composer_empty => {
                        app.hitl_grant_confirm = None;
                        decide_hitl(app, tx, true)
                    }
                    // Session-wide grants are two-press: the first arms the
                    // confirm, the second commits. One keystroke must never
                    // hand out blanket approval.
                    //
                    // The arming press ALSO types its character, so someone
                    // starting the message "add the test" keeps every letter —
                    // arming is invisible to them and the next key disarms it.
                    // The confirming press takes that character back out.
                    KeyCode::Char(c @ ('a' | 'A'))
                        if composer_empty || app.hitl_grant_confirm == Some(c) =>
                    {
                        if app.hitl_grant_confirm == Some(c) {
                            app.hitl_grant_confirm = None;
                            app.input.delete_char();
                            if c == 'a' {
                                if let Some(req) = &app.hitl {
                                    app.session_tool_allow.insert(req.tool_name.clone());
                                }
                            } else {
                                app.auto_approve = true;
                            }
                            decide_hitl(app, tx, true);
                        } else {
                            app.hitl_grant_confirm = Some(c);
                            app.input.input(key);
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') if composer_empty => {
                        app.hitl_grant_confirm = None;
                        decide_hitl(app, tx, false)
                    }
                    KeyCode::Esc => {
                        app.hitl_grant_confirm = None;
                        decide_hitl(app, tx, false)
                    }
                    // No submit while the modal is open, but still disarm: the
                    // modal promises "any other key cancels".
                    KeyCode::Enter => app.hitl_grant_confirm = None,
                    _ => {
                        app.hitl_grant_confirm = None;
                        app.input.input(key);
                    }
                }
                return;
            }
            // Stale-decision swallow: a gate that auto-resolves (read lane,
            // /auto, session allow) the moment the operator presses a decision
            // key would otherwise send that keystroke into the composer as
            // text. For a short window after any resolution, eat one more
            // decision key so the composer stays clean.
            if let Some(t) = app.hitl_resolved_at
                && t.elapsed() < std::time::Duration::from_millis(800)
                && matches!(
                    key.code,
                    KeyCode::Char('y' | 'Y' | 'a' | 'A' | 'n' | 'N') | KeyCode::Esc
                )
            {
                return;
            }
            if key.code != KeyCode::Esc {
                app.last_esc_at = None;
                app.esc_hint = false;
            }
            // Any keypress other than a repeat Ctrl+C disarms the quit
            // confirmation, mirroring the Esc arm/hint reset above.
            let is_ctrl_c = ctrl && matches!(key.code, KeyCode::Char('c'));
            if !is_ctrl_c {
                app.last_ctrl_c_at = None;
                app.ctrl_c_hint = false;
            }
            let shift = key.modifiers.contains(KeyModifiers::SHIFT);
            // Alt/Option+Enter is a universal newline fallback: legacy
            // terminals report Alt via a plain ESC prefix with no protocol
            // opt-in, unlike Shift which needs the (not-universally-supported)
            // keyboard-enhancement protocol pushed in `TerminalGuard::enter`.
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            // While the completion menu is open it owns navigation / accept /
            // dismiss keys; everything else falls through to normal editing and
            // re-filters the menu at the end of this handler.
            if app.completion.is_some() {
                match key.code {
                    // Chooser (suggested replies): a digit picks that option and
                    // sends it straight away — the fast path fzf/gum/Claude Code
                    // all offer. Only in `spaced` mode and only for an in-range
                    // index, so digit-typing still works in the slash menu.
                    KeyCode::Char(d @ '1'..='9')
                        if !ctrl
                            && !alt
                            && app.completion.as_ref().is_some_and(|c| {
                                c.spaced && (d as usize - '1' as usize) < c.items.len()
                            }) =>
                    {
                        let idx = d as usize - '1' as usize;
                        if let Some(c) = app.completion.as_mut() {
                            c.selected = idx;
                        }
                        let sends = app
                            .completion
                            .as_ref()
                            .and_then(|c| c.items.get(idx))
                            .is_some_and(|cand| !cand.has_children);
                        completion_accept(app);
                        if sends {
                            submit(app, tx).await;
                        }
                        return;
                    }
                    // Ctrl+↑/↓ resizes the chooser band (agent chooser only).
                    KeyCode::Up if ctrl && app.completion.as_ref().is_some_and(|c| c.spaced) => {
                        app.chooser_grow = app.chooser_grow.saturating_add(1);
                        return;
                    }
                    KeyCode::Down if ctrl && app.completion.as_ref().is_some_and(|c| c.spaced) => {
                        app.chooser_grow = app.chooser_grow.saturating_sub(1);
                        return;
                    }
                    KeyCode::Up => {
                        completion_move(app, -1);
                        return;
                    }
                    KeyCode::Down => {
                        completion_move(app, 1);
                        return;
                    }
                    KeyCode::Char('p') if ctrl => {
                        completion_move(app, -1);
                        return;
                    }
                    KeyCode::Char('n') if ctrl => {
                        completion_move(app, 1);
                        return;
                    }
                    KeyCode::Tab => {
                        completion_accept(app);
                        return;
                    }
                    KeyCode::Enter => {
                        // Enter accepts the candidate; if it is a leaf (no
                        // submenu) we also send right away instead of forcing a
                        // second Enter.
                        let sends = app
                            .completion
                            .as_ref()
                            .and_then(|c| c.items.get(c.selected))
                            .is_some_and(|cand| !cand.has_children);
                        completion_accept(app);
                        if sends {
                            submit(app, tx).await;
                        }
                        return;
                    }
                    KeyCode::Esc => {
                        app.completion = None;
                        return;
                    }
                    _ => {}
                }
            }
            // Agent ghost suggestion: Tab fills it when the composer is empty.
            if app.suggestion_ghost.is_some()
                && key.code == KeyCode::Tab
                && app.input_text().is_empty()
            {
                if let Some(s) = app.suggestion_ghost.take() {
                    app.set_input(&s);
                }
                return;
            }
            match key.code {
                KeyCode::Char('d') if ctrl => request_quit(app, tx),
                KeyCode::Char('c') if ctrl => handle_ctrl_c(app, tx),
                KeyCode::Char('u') if ctrl => app.clear_input(),
                KeyCode::Char('v') if ctrl => {
                    if !attach_clipboard_image(app) {
                        app.push_system(
                            "no image in clipboard — copy a screenshot then Ctrl+V (or Cmd+V / drag an image file)",
                        );
                    }
                }
                KeyCode::Char('o') if ctrl => {
                    scrollback_dump(app);
                }
                // Ctrl+T (moniTor) works on every terminal; Alt+Monitor is the
                // better mnemonic but on macOS types a literal 'µ' unless
                // Option-as-Meta is enabled, so both are bound to the same
                // handler — whichever the terminal actually delivers. Never
                // Ctrl+M: `^M` IS Enter (carriage return) on every terminal,
                // so binding it would submit the half-typed message instead
                // of opening the monitor list.
                KeyCode::Char('t') if ctrl => {
                    monitor::handle(app, &[], tx).await;
                }
                KeyCode::Char('m' | 'M') if alt => {
                    monitor::handle(app, &[], tx).await;
                }
                // Ctrl+R — re-run the request whose approval expired (#8):
                // refill the composer from the stashed `expired_retry` so the
                // user can resend with one key. Only when something is stashed
                // and the composer is empty, so it never clobbers a draft.
                KeyCode::Char('r') if ctrl => {
                    if app.input_text().is_empty()
                        && let Some(text) = app.expired_retry.take()
                    {
                        app.set_input(&text);
                    }
                }
                KeyCode::PageUp => {
                    app.scroll_back = app.scroll_back.saturating_add(app.scroll_page.max(1))
                }
                KeyCode::PageDown => {
                    app.scroll_back = app.scroll_back.saturating_sub(app.scroll_page.max(1))
                }
                KeyCode::Tab => refresh_completion(app),
                KeyCode::Enter if shift || alt => {
                    app.input.insert_newline();
                }
                KeyCode::Enter => submit(app, tx).await,
                // Shell-style input history: ↑ on the first composer line
                // recalls older sent messages; ↓ on the last line walks
                // newer / restores the stashed draft. Anywhere else the
                // arrows keep moving the cursor.
                KeyCode::Up if app.input.cursor().0 == 0 => {
                    if !app.history_prev() {
                        app.input.input(key);
                    }
                }
                KeyCode::Down if app.input.cursor().0 + 1 == app.input.lines().len() => {
                    if !app.history_next() {
                        app.input.input(key);
                    }
                }
                KeyCode::Esc => {
                    let action =
                        esc_action(app.last_esc_at, app.streaming, app.input_text().is_empty());
                    match action {
                        EscAction::Arm => {
                            app.last_esc_at = Some(std::time::Instant::now());
                            app.esc_hint = true;
                        }
                        EscAction::ClearInput => {
                            app.clear_input();
                            app.last_esc_at = None;
                            app.esc_hint = false;
                        }
                        EscAction::CancelAndRestore => {
                            cancel_in_flight(app, tx);
                            // Only repopulate the composer from `last_sent`
                            // when it is empty. If the user typed a steer
                            // draft mid-turn and then double-ESC'd, that draft
                            // is what they want back — overwriting it with an
                            // older `last_sent` stranded stale text in the box
                            // that no later turn cleared (the "leftover
                            // steering line" bug).
                            if app.input_text().is_empty()
                                && let Some(text) = app.last_sent.clone()
                            {
                                app.set_input(&text);
                            }
                            app.push_system("cancelled — message restored");
                            app.last_esc_at = None;
                            app.esc_hint = false;
                        }
                        EscAction::Nothing => {
                            app.last_esc_at = None;
                            app.esc_hint = false;
                        }
                    }
                }
                _ => {
                    app.input.input(key);
                }
            }
            refresh_completion(app);
        }
        Event::Paste(text) => {
            // How Cmd+V image paste works: the terminal eats Cmd+V and pastes
            // the clipboard as bracketed text. For an image it pastes either the
            // temp-file PATH it wrote (iTerm2/most) or nothing/whitespace. So:
            //   image-file path  → load that file (covers Cmd+V & drag-drop)
            //   empty/whitespace → try the clipboard image (covers raw paste)
            //   otherwise        → ordinary text paste
            let trimmed = text.trim();
            if let Some((mime, b64)) = paste::image_from_paste(trimmed) {
                stage_image(app, mime, b64);
            } else if trimmed.is_empty() {
                // An empty bracketed paste is the terminal's signal for "the
                // clipboard has content but no text to give you" — try
                // reading an image off it directly. Previously a failed
                // read here fell through to `insert_str("")`, a silent
                // no-op with zero feedback; now it reports the same way
                // Ctrl+V does on the identical failure.
                if !attach_clipboard_image(app) {
                    app.push_system(
                        "paste looked like an image but the clipboard had none — copy a screenshot first",
                    );
                }
            } else {
                app.input.insert_str(text);
            }
            refresh_completion(app);
        }
        Event::Mouse(mouse_ev) => match mouse_ev.kind {
            MouseEventKind::ScrollUp => {
                app.scroll_back = app.scroll_back.saturating_add(MOUSE_SCROLL_STEP);
            }
            MouseEventKind::ScrollDown => {
                app.scroll_back = app.scroll_back.saturating_sub(MOUSE_SCROLL_STEP);
            }
            _ => {}
        },
        Event::FocusGained => app.focused = true,
        Event::FocusLost => app.focused = false,
        _ => {}
    }
}

/// Which `Ctrl+<char>` combinations the key-dispatch match above binds.
/// Hand-maintained beside those arms (there is no way to introspect a
/// `match` at runtime) so a test can assert `Ctrl+M` is never one of them —
/// `^M` IS Enter on every terminal, so binding it would shadow submitting a
/// message — without having to drive the whole event loop. Test-only: there
/// is no other caller.
#[cfg(test)]
pub(super) fn binds_ctrl(c: char) -> bool {
    matches!(c, 'd' | 'c' | 'u' | 'v' | 'o' | 't' | 'r')
}

/// Whether `Alt+m`/`Alt+M` reaches the monitor handler — the mnemonic
/// alternative to `Ctrl+T`, safe to bind because a stray `Alt+M` on a
/// terminal without Option-as-Meta just types a literal 'µ' (one backspace),
/// unlike `Ctrl+M` which IS carriage return. Test-only: there is no other
/// caller.
#[cfg(test)]
pub(super) fn binds_alt_m() -> bool {
    true
}
