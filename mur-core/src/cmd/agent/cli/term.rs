//! Terminal guard, TUI setup, viewport and handover, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;

/// RAII terminal restore — runs on every exit path including unwind.
pub(super) struct TerminalGuard {
    /// Whether the terminal advertised keyboard-enhancement support at enter().
    /// Queried exactly once here so Drop can reuse it instead of re-querying —
    /// a second query leaks a stray capability-response into the shell on exit.
    kbd_enhanced: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        // No EnterAlternateScreen and no EnableMouseCapture here: the TUI
        // starts on the MAIN screen with a small fixed-height inline
        // viewport, so native scrollback and native mouse-drag text
        // selection both work untouched. `sync_surface` enters the
        // alt-screen only for heavy overlays (Ctrl+O, /mcp, /skill), which
        // is a fire-and-forget mode-set escape, not a query — safe to toggle
        // even with the async `EventStream` reading stdin concurrently.
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)
            .context("enable terminal modes")?;
        let kbd_enhanced = matches!(supports_keyboard_enhancement(), Ok(true));
        push_keyboard_enhancement(kbd_enhanced);
        Ok(Self { kbd_enhanced })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        pop_keyboard_enhancement(self.kbd_enhanced);
        if ON_ALT.swap(false, Ordering::Relaxed) {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableFocusChange,
            cursor::Show
        );
        let _ = disable_raw_mode();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_tui(
    home: PathBuf,
    agent: String,
    resume: bool,
    auto: bool,
    skin: Option<String>,
    budget_usd: Option<f64>,
    auto_reads: bool,
    fleet: Option<String>,
) -> Result<()> {
    // Resolve skin: CLI flag > config > "ansi"
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    let skin_name = skin
        .as_deref()
        .or(cfg.cli.skin.as_deref())
        .unwrap_or("ansi");
    let unknown_skin = !theme::is_known_skin(skin_name);
    let active_theme = theme::resolve_skin(skin_name);

    // Restore the terminal even if a later panic unwinds past the guard.
    // Capture enhancement support once so the panic path doesn't re-query the
    // terminal (which would leak a capability-response after the crash dump).
    let kbd_enhanced = matches!(supports_keyboard_enhancement(), Ok(true));
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        pop_keyboard_enhancement(kbd_enhanced);
        if ON_ALT.swap(false, Ordering::Relaxed) {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableFocusChange,
            cursor::Show
        );
        let _ = disable_raw_mode();
        prev_hook(info);
    }));

    let _guard = TerminalGuard::enter()?;

    let mut app = build_app(&home, &agent, resume, active_theme)?;
    if let Some(f) = fleet.as_deref() {
        app.fleet = Some(fleet_rail::FleetRail::start(f));
    }
    app.skills = complete::load_agent_skills(&agent);
    app.menu_ctx = complete::MenuContext::load(&home, &agent);
    let (initial_pricing, book) = load_pricing(&home, &agent);
    app.pricing = initial_pricing;
    app.pricing_book = Some(book);
    if unknown_skin {
        app.push_system(format!(
            "unknown skin '{skin_name}', using ansi — valid: {}",
            theme::SKIN_NAMES
        ));
    }
    // Auto-approve is the default and the status bar's AUTO badge says so;
    // only the opt-out gets a notice, because a session that asks is the one
    // whose operator has to know why it stopped.
    app.auto_approve = auto;
    if !auto {
        app.push_system(
            "ask-first is ON for this session (--ask) — every tool call waits for you; /auto to approve without asking",
        );
    }
    app.budget_usd = budget_usd;
    if let Some(b) = budget_usd {
        app.push_system(format!(
            "session budget ${b:.2} — new turns stop once estimated spend reaches it"
        ));
    }
    app.auto_reads = auto_reads;
    if auto_reads {
        app.push_system("auto-reads is ON — read-only bash commands (cat/ls/grep/git status/…) are auto-approved; writes and ambiguous commands still prompt");
    }

    // Fixed height for the terminal's current size — see `viewport_h_for` for
    // why the viewport is never resized while the session runs (the event loop
    // only rebuilds it when the TERMINAL resizes, or when the welcome gives way
    // to the first message). Built AFTER the app because "is this the welcome?"
    // is a question about the transcript.
    let initial_h = crossterm::terminal::size()
        .map(|(_, rows)| viewport_h_for(rows))
        .unwrap_or(INLINE_VIEWPORT_HEIGHT);
    // The welcome is terminal output, printed at the cursor like any banner:
    // mascot at the top of what follows, and the viewport anchored below it.
    if app.welcome_applies() {
        let _ = welcome::print_banner(&mut io::stdout(), &app.welcome_banner());
    }
    // Anchor the viewport at the BOTTOM of the screen, like `purge_and_reanchor`
    // does: `with_options` anchors wherever the cursor happens to be, which on a
    // tall window pins the composer a fifth of the way down with dead space
    // below it. Bottom-anchoring also gives `insert_before` the headroom it
    // wants for the first screenful of transcript. Only ever move DOWN — moving
    // up would draw the viewport over visible shell output.
    if let Ok((_, rows)) = crossterm::terminal::size()
        && let Some(row) = anchor_row(rows, initial_h, cursor::position().ok().map(|(_, r)| r))
    {
        let _ = execute!(io::stdout(), cursor::MoveTo(0, row));
    }
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(initial_h),
        },
    )
    .context("init terminal")?;

    let cwd = app.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    let (panel_rx, panel_handle) = panel::start(&app.home, &app.agent, &cwd);
    app.panel = Some(panel_handle);
    let result = event_loop(&mut terminal, &mut app, panel_rx).await;

    drop(_guard);
    let _ = terminal.show_cursor();
    result
}

pub(super) fn build_app(
    home: &Path,
    agent: &str,
    resume: bool,
    theme: &'static theme::Theme,
) -> Result<App> {
    if resume {
        if let Some(info) = persist::latest(home, agent)? {
            let turns = persist::load(home, &info.id, agent)?;
            let mut app = App::new(
                home.to_path_buf(),
                agent.to_string(),
                Session::open_existing(home, agent, &info.id)?,
                theme,
            );
            app.load_history(turns);
            app.refresh_channel();
            app.push_system(format!(
                "resumed conversation ({} turns) · type /help for commands",
                app.messages.len()
            ));
            return Ok(app);
        }
        let mut app = App::new(
            home.to_path_buf(),
            agent.to_string(),
            Session::create(home, agent)?,
            theme,
        );
        app.push_system(
            "no saved conversation to resume; starting fresh · type /help for commands".to_string(),
        );
        return Ok(app);
    }
    let app = App::new(
        home.to_path_buf(),
        agent.to_string(),
        Session::create(home, agent)?,
        theme,
    );
    // No startup HELP dump: an empty transcript renders the welcome screen
    // (mascot + identity + one example + /help hint). The full cheatsheet stays
    // reachable via the /help command (SlashCmd::Help below).
    Ok(app)
}

/// Bring the physical terminal surface in line with `app.render_mode`.
///
/// Entering/leaving the alternate screen is a fire-and-forget mode-set
/// escape code — unlike `Terminal::resize`, it needs no reply from the
/// terminal, so it's safe to call here even with the async `EventStream`
/// concurrently reading the same stdin. This only performs the transition
/// exactly on the edges (recording the surface it last applied via
/// `ON_ALT`), so repeated calls are cheap no-ops; `needs_full_redraw` makes
/// the next `terminal.clear()` repaint the fresh surface.
pub(super) fn sync_surface(app: &mut App) -> Result<()> {
    let want_alt = app.render_mode == RenderMode::Fullscreen;
    let on_alt = ON_ALT.load(Ordering::Relaxed);
    if want_alt == on_alt {
        return Ok(());
    }
    if want_alt {
        execute!(io::stdout(), EnterAlternateScreen)?;
    } else {
        execute!(io::stdout(), LeaveAlternateScreen)?;
    }
    ON_ALT.store(want_alt, Ordering::Relaxed);
    app.needs_full_redraw = true;
    Ok(())
}

/// Return from a heavy overlay to the inline chat surface. Idempotent.
pub(super) fn leave_fullscreen(app: &mut App) {
    if app.render_mode == RenderMode::Inline {
        return;
    }
    app.render_mode = RenderMode::Inline;
    app.needs_full_redraw = true;
}

/// The Inline viewport height for a terminal `rows` tall — a CONSTANT for the
/// life of a session (it only changes when the terminal itself resizes).
///
/// Fixed on purpose. A resize of the viewport can only be anchored one of two
/// ways, and both are worse than reserving the rows: keeping the old TOP row
/// leaves the composer floating above the screen bottom until enough content
/// scrolls in to fill the freed rows (`insert_before` only re-anchors at the
/// bottom when it actually has to scroll), and anchoring the new BOTTOM leaves
/// a blank hole above the viewport that the next `insert_before` pushes
/// straight into scrollback (#728's growing gap). With the height fixed, the
/// composer sits at `rows - input_h - 1` forever and `ui::flush_finished`
/// keeps the band full of real content instead, so the reserve costs nothing.
///
/// Never as tall as the screen: a full-height viewport forces `insert_before`
/// through its degenerate whole-screen path (draw over the top + full scroll +
/// clear + repaint), which leaks stale frame copies into scrollback and bleeds
/// old glyphs through the status row on short windows. One spare row keeps the
/// healthy region-scroll paths in play.
///
/// The welcome used to be the one exception — a full-window viewport so the
/// mascot sat at the top and the composer on the floor. The first message
/// then had to shrink it, and the only way to shrink an Inline viewport is a
/// purge plus a re-anchor: on a tall terminal the whole conversation, mascot
/// included, dropped to the bottom fifth of the window with a void above it.
/// The welcome is now *printed* above the viewport (`welcome::print_banner`)
/// instead of painted inside it, so one height serves both surfaces, the
/// viewport is bottom-anchored from the first frame, and nothing ever
/// re-anchors.
/// Where to park the cursor before `Terminal::with_options` anchors the Inline
/// viewport there. `None` = leave it where it is.
///
/// `with_options` anchors at the cursor and scrolls only as far as it must, so
/// the cursor's row decides where the viewport lands:
///
/// - **Above the anchor row** — move down to it, so the composer sits on the
///   floor of the window from the first frame. The welcome banner has already
///   been printed above by then, so nothing is drawn over it. Moving *up*
///   would draw the viewport over shell output that is still on screen (and
///   not scroll it, so it would not reach scrollback either), which is why
///   this only ever descends.
/// - **At or below it** — leave it: `with_options` scrolls what it needs and
///   the viewport lands on the bottom rows by itself.
/// - **Unknown** (the cursor-position query failed — it is a terminal
///   round-trip and can) — park on the last row. Leaving it put anchors the
///   viewport wherever the shell's cursor happened to be, and every row BELOW
///   the viewport then keeps its pre-murmur contents: old shell output
///   stranded under the composer, which nothing ever repaints because ratatui
///   owns only the viewport rows. The last row instead makes `with_options`
///   scroll the screen to make room, so displaced rows reach scrollback intact
///   and the viewport is bottom-anchored — the invariant `purge_and_reanchor`
///   maintains everywhere else.
pub(super) fn anchor_row(rows: u16, viewport_h: u16, cursor_row: Option<u16>) -> Option<u16> {
    let top = rows.saturating_sub(viewport_h);
    match cursor_row {
        Some(r) if r < top => Some(top),
        Some(_) => None,
        None => Some(rows.saturating_sub(1)),
    }
}

pub(super) fn viewport_h_for(rows: u16) -> u16 {
    rows.saturating_sub(1).clamp(5, INLINE_VIEWPORT_HEIGHT)
}

/// Rebuild the terminal after its size changed (font zoom / window resize).
///
/// A terminal reflow moves the Inline viewport's rows in ways we cannot
/// track, so any in-place fix leaves a stale copy of the old viewport in
/// scrollback (ratatui's own `autoresize` leaks the same way). The only
/// deterministic recovery is scorched earth: wait for the size to settle
/// (font zoom fires one resize per keypress), wipe the screen AND
/// scrollback, re-anchor a fresh viewport at the top, and reset
/// `flushed_upto` so the next `flush_finished` re-emits the whole
/// transcript wrapped at the new width. Caller must drop any live
/// `EventStream` first: re-anchoring reads the terminal's cursor-position
/// response from stdin.
pub(super) fn rebuild_after_resize(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<u16> {
    // Let the reflow storm settle so we rebuild once, not per event.
    let mut size = crossterm::terminal::size()?;
    loop {
        std::thread::sleep(Duration::from_millis(80));
        let now = crossterm::terminal::size()?;
        if now == size {
            break;
        }
        size = now;
    }
    // Re-derive the height from the SETTLED size: re-anchoring with the
    // pre-resize height on a now-shorter terminal would let the viewport fill
    // the whole screen, which sends `insert_before` through its degenerate
    // draw-over-the-top path (lost/garbled scrollback rows). A resize is the
    // only time the viewport height changes at all.
    let h = viewport_h_for(size.1);
    let banner = app.welcome_applies().then(|| app.welcome_banner());
    purge_and_reanchor(terminal, h, banner.as_deref())?;
    app.flushed_upto = 0;
    app.flushed_bytes = 0;
    Ok(h)
}

/// The app-state half of starting a terminal handover: push the notice, cancel
/// any pending screen wipe, and report the viewport height the handover needs.
/// Split out from the loop so the policy — most of which is a *refusal* to do
/// something — is testable at all.
///
/// **It must not touch `flushed_upto`/`flushed_bytes`.** `rebuild_after_resize`
/// (above) and `App::start_new_session` both reset them, but each erases what
/// is already on screen first — `purge_and_reanchor` in one, `messages.clear()`
/// in the other — which is what makes re-emitting from index 0 a redraw. The
/// handover path purges nothing on purpose: `handover::reanchor` is the
/// no-`Purge` variant precisely so the login transcript survives. Resetting the
/// cursors here would make the next `flush_finished` take `start = 0` and
/// `insert_before` the whole settled transcript a second time, burying the
/// login transcript under a duplicate of the conversation.
///
/// `wants_screen_wipe` is cleared for the same reason the reset is skipped: a
/// wipe left pending would run `purge_and_reanchor` on the pass after the
/// child exits and take the transcript with it.
pub(super) fn prepare_handover(app: &mut App, label: &str, term_rows: u16) -> u16 {
    app.push_system(format!("{label}: handing over the terminal…"));
    app.wants_screen_wipe = false;
    viewport_h_for(term_rows)
}

/// Wipe the screen AND scrollback, then re-anchor a fresh Inline viewport
/// of height `h` anchored at the BOTTOM of the screen. Shared by the resize
/// rebuild and the /clear / channel-switch screen wipe. Caller must drop any
/// live `EventStream` first.
///
/// Bottom-anchored on purpose: `with_options` anchors the viewport at the
/// cursor, and a viewport at row 0 has no headroom above it, so the next
/// transcript replay's `insert_before` must draw THROUGH the viewport rows
/// and scroll the whole screen — any row overwritten before its scroll never
/// reaches scrollback intact (bodies went missing right after a resize).
/// Anchoring at the bottom leaves the headroom `insert_before` needs to lay
/// replayed rows down above the viewport, which is also the steady state the
/// UI migrates to anyway.
pub(super) fn purge_and_reanchor(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    h: u16,
    banner: Option<&[ratatui::text::Line<'static>]>,
) -> Result<()> {
    use crossterm::cursor::MoveTo;
    use crossterm::terminal::{Clear, ClearType};
    let rows = crossterm::terminal::size()?.1;
    crossterm::execute!(
        io::stdout(),
        MoveTo(0, 0),
        Clear(ClearType::All),
        Clear(ClearType::Purge),
    )?;
    // The welcome, when it still applies, goes back at the top as terminal
    // output; the viewport then re-anchors on the floor (#725: anchoring it
    // at the top leaves `insert_before` no headroom and drops replayed rows).
    if let Some(lines) = banner {
        welcome::print_banner(&mut io::stdout(), lines)?;
    }
    crossterm::execute!(io::stdout(), MoveTo(0, rows.saturating_sub(h)))?;
    // The cursor-position query inside `with_options` needs crossterm's
    // internal event reader; the just-dropped EventStream's background
    // thread can hold that lock for a beat longer. Retry briefly.
    let mut last_err = None;
    for _ in 0..20 {
        match Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Inline(h),
            },
        ) {
            Ok(t) => {
                *terminal = t;
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Err(last_err.unwrap()).context("reanchor terminal")
}
