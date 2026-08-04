//! `mur agent cli <name>` — interactive streaming TUI chat with an agent.
//!
//! This is a terminal front-end over the already-working A2A streaming client
//! (`crate::a2a_dial::dial_message_streaming`); it adds no protocol surface. See
//! the sibling modules: [`stream`] (blocking-dial ↔ async bridge), [`app`]
//! (state), [`ui`] (ratatui render), [`markdown`] (reply rendering), and
//! [`persist`] (JSONL session log + resume).

mod access;
mod app;
mod bash_class;
mod complete;
mod diff;
mod dump;
mod fleet_rail;
mod follow;
mod footer;
mod manage;
mod markdown;
mod memory_cmds;
mod multiplex;
mod panel;
mod paste;
pub mod persist;
mod recover;
mod render_card;
mod step;
mod stream;
mod suggest;
mod theme;
mod ui;
mod welcome;

use std::io::{self, BufRead, IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::time::Instant as StdInstant;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange, Event,
    EventStream, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use crossterm::{cursor, execute};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::Instant as TokioInstant;

use self::app::{
    App, ESC_DOUBLE_WINDOW, EscAction, OverlayKeyAction, RenderMode, Role, SlashCmd,
    arm_input_debounce, esc_action, overlay_key_action, parse_slash, take_due_input,
};
use self::persist::Session;
use self::stream::{StreamMsg, build_params, cancel_task, respond_hitl, spawn_stream};
use crate::a2a_dial::{DialMode, canonicalize_agent_name, dial_method};

/// Load the agent's model pricing from `~/.mur/models.yaml`. Falls back to
/// `Pricing::default()` (all `None` fields) on any error or when the agent
/// uses an inline model rather than a `model_ref:` registry alias. The footer
/// renderer treats `None` costs/window as "unknown" and shows `—`.
fn load_pricing(home: &std::path::Path, agent: &str) -> footer::Pricing {
    let pricing = footer::Pricing::default();
    let Ok((_, profile)) = crate::cmd::agent::load_profile_for_edit(agent) else {
        return pricing;
    };
    let Some(model_ref) = profile.model_ref else {
        return pricing;
    };
    let reg_path = home.join("models.yaml");
    let Ok(reg) = mur_common::model::ModelRegistry::load_from(&reg_path) else {
        return pricing;
    };
    let Some(entry) = reg.models.get(&model_ref) else {
        return pricing;
    };
    let (input, output) = entry.effective_costs();
    footer::Pricing {
        in_per_1k: input,
        out_per_1k: output,
        window: entry.context_window,
    }
}

/// How many recent conversations `/sessions` lists.
const RECENT_LIMIT: usize = 10;
/// Mouse wheel scrolls one line per event (trackpads fire 10-20 events/sec, so
/// per-line granularity stays smooth); PageUp/PageDown move a full screenful.
const MOUSE_SCROLL_STEP: u16 = 1;
/// Fixed height of the Inline-mode viewport: 8 (max composer lines) + 2
/// (composer border) + 1 (status line) + 9 (tail preview of the
/// currently-streaming reply, plus its own top/bottom border). Generous
/// enough that the common case (short composer, short-to-medium reply)
/// never scrolls within its own area; a very long streaming reply just
/// shows its latest lines until it finishes and flushes to scrollback.
const INLINE_VIEWPORT_HEIGHT: u16 = 20;
/// Spinner animation cadence.
const SPINNER_MS: u64 = 90;
/// Max chars of an arg hint shown on a step line in `--plain` mode.
const PLAIN_STEP_HINT_MAX: usize = 120;

const HELP: &str = "commands: /help  /clear (new conversation)  /card  /sessions  /channels [N] (list/switch)  /channels N --follow (live-tail another channel; /channels --follow to stop)  /auto [on|off]  /verbose [on|off] (expand tool cards)  /skin [dark|light|mur]  /mcp  /skill  /remember <text> (save a memory)  /memories  /forget <name|last>  /panel [tab]  /exit · !cmd runs a local shell command (output shared with the agent) · keys: Enter send · Shift+Enter newline · Ctrl+V attach screenshot · Ctrl+C cancel/clear · Ctrl+D quit · PageUp/PageDown scroll";

/// Entry point dispatched from `AgentAction::Cli`.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_cli(
    names: &[String],
    resume: bool,
    auto: bool,
    skin: Option<String>,
    plain: bool,
    budget_usd: Option<f64>,
    auto_reads: bool,
    fleet: Option<String>,
) -> Result<()> {
    if names.len() > 1 {
        if budget_usd.is_some() {
            eprintln!(
                "note: --budget-usd is only enforced in the single-agent TUI; it is ignored when opening multiple agents."
            );
        }
        if auto_reads {
            eprintln!(
                "note: --auto-reads is only enforced in the single-agent TUI; it is ignored when opening multiple agents."
            );
        }
        if fleet.is_some() {
            eprintln!(
                "note: --fleet is only shown in the single-agent TUI; it is ignored when opening multiple agents."
            );
        }
        let names = names.to_vec();
        return tokio::task::spawn_blocking(move || multiplex::run(&names, resume, auto)).await?;
    }
    let name = names.first().context("at least one agent name required")?;
    let home = super::resolve_mur_home()?;

    // Fail loudly on an unknown fleet. Degrading to a plain murmur would leave
    // the user believing they are watching a fleet when they are not.
    if let Some(f) = fleet.as_deref() {
        crate::cmd::fleet::store::load_fleet(&home, f).with_context(|| format!("--fleet {f}"))?;
    }

    let agent = canonicalize_agent_name(&home, name);

    // Streaming requires a live socket; fail early with a friendly hint.
    let lock = home.join("agents").join(&agent).join("running.lock");
    if !lock.exists() {
        eprintln!(
            "Agent '{agent}' is not running. Start it first with:\n    mur agent start {agent}\nthen retry: mur agent cli {agent}"
        );
        return Ok(());
    }

    // If this project dir is outside the agent's filesystem grants, offer to
    // add it (explicit consent, persisted; sandbox applies it on next restart).
    access::ensure_cwd_access(&agent)?;

    // Plain line mode: forced by --plain, or automatic when stdout is not a
    // terminal (piped / CI). `interactive` drives the prompt + HITL behaviour:
    // a real stdin TTY gets an echoed prompt and a [y/a/n] HITL question; a
    // pipe gets neither.
    if plain || !io::stdout().is_terminal() {
        if budget_usd.is_some() {
            eprintln!(
                "note: --budget-usd is only enforced in the interactive TUI; it is ignored in plain/piped mode."
            );
        }
        if auto_reads {
            eprintln!(
                "note: --auto-reads is only enforced in the interactive TUI; it is ignored in plain/piped mode."
            );
        }
        if fleet.is_some() {
            eprintln!(
                "note: --fleet is only shown in the interactive TUI; it is ignored in plain/piped mode."
            );
        }
        let home2 = home.clone();
        let agent2 = agent.clone();
        let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
        return tokio::task::spawn_blocking(move || run_plain(&home2, &agent2, auto, interactive))
            .await?;
    }

    run_tui(
        home, agent, resume, auto, skin, budget_usd, auto_reads, fleet,
    )
    .await
}

// ── TUI mode ────────────────────────────────────────────────────────────────

/// Try to enable disambiguated escape codes so Shift+Enter (and other
/// modified keys) are reported with a distinct modifier instead of looking
/// like a bare keypress. Not every terminal supports this protocol (e.g.
/// macOS Terminal.app) — silently skip there; Alt/Option+Enter remains a
/// universal fallback for the newline binding since legacy terminals already
/// report Alt via an ESC prefix with no protocol opt-in required.
// Remembers whether the push below actually activated the protocol (i.e. the
// terminal both advertised support and the enable escape was written
// successfully), so pop never has to re-derive that itself.
static KB_ENHANCEMENT_ACTIVE: AtomicBool = AtomicBool::new(false);

fn push_keyboard_enhancement(supported: bool) {
    if supported
        && execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )
        .is_ok()
    {
        KB_ENHANCEMENT_ACTIVE.store(true, Ordering::Relaxed);
    }
}

// Callers pass their own cached `supports_keyboard_enhancement()` result (see
// `TerminalGuard`/panic hook/`scrollback_dump`) purely so *they* don't have to
// re-query the terminal — but the actual decision to pop is driven by
// `KB_ENHANCEMENT_ACTIVE`, not by that flag. Deliberately does NOT call
// `supports_keyboard_enhancement()` itself: that sends a second terminal
// query-and-wait (up to crossterm's 2s timeout). If the reply arrives after
// we've already disabled raw mode / left the alternate screen, nothing reads
// it — the raw escape bytes fall through to the shell's cooked-mode stdin and
// get echoed as garbage (e.g. `^[[?1u^[[?62;22;52c`). We already know from the
// push above whether the protocol is actually active, so just use that.
fn pop_keyboard_enhancement(_supported: bool) {
    if KB_ENHANCEMENT_ACTIVE.swap(false, Ordering::Relaxed) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
}

/// Tracks whether the physical terminal is currently on the alternate screen.
/// Owned here (not inside `sync_surface`) so the RAII guard's `Drop` and the
/// panic hook can decide whether a `LeaveAlternateScreen` is actually needed —
/// Inline mode never entered the alt-screen, so leaving it would corrupt the
/// user's scrollback on exit. `sync_surface` is the only writer during normal
/// operation; the guard/panic paths only read.
static ON_ALT: AtomicBool = AtomicBool::new(false);

/// RAII terminal restore — runs on every exit path including unwind.
struct TerminalGuard {
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
async fn run_tui(
    home: PathBuf,
    agent: String,
    resume: bool,
    auto: bool,
    skin: Option<String>,
    budget_usd: Option<f64>,
    auto_reads: bool,
    fleet: Option<String>,
) -> Result<()> {
    // Resolve skin: CLI flag > config > "dark"
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    let skin_name = skin
        .as_deref()
        .or(cfg.cli.skin.as_deref())
        .unwrap_or("dark");
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
    app.pricing = load_pricing(&home, &agent);
    if unknown_skin {
        app.push_system(format!(
            "unknown skin '{skin_name}', using dark — valid: dark, light, mur"
        ));
    }
    if auto {
        app.auto_approve = true;
        app.push_system("auto-approve is ON for this session (--auto) — every tool call will be allowed without asking");
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
        .map(|(_, rows)| viewport_h_for(rows, app.messages.is_empty()))
        .unwrap_or(INLINE_VIEWPORT_HEIGHT);
    // Anchor the viewport at the BOTTOM of the screen, like `purge_and_reanchor`
    // does: `with_options` anchors wherever the cursor happens to be, which on a
    // tall window pins the composer a fifth of the way down with dead space
    // below it. Bottom-anchoring also gives `insert_before` the headroom it
    // wants for the first screenful of transcript. Only ever move DOWN — moving
    // up would draw the viewport over visible shell output.
    if let Ok((_, rows)) = crossterm::terminal::size() {
        let top = rows.saturating_sub(initial_h);
        if cursor::position().is_ok_and(|(_, r)| r < top) {
            let _ = execute!(io::stdout(), cursor::MoveTo(0, top));
        }
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

fn build_app(home: &Path, agent: &str, resume: bool, theme: &'static theme::Theme) -> Result<App> {
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
fn sync_surface(app: &mut App) -> Result<()> {
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
fn leave_fullscreen(app: &mut App) {
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
/// `welcome` is the one exception: an empty transcript paints the mascot and
/// nothing is ever flushed (there is no transcript to flush), so the viewport
/// takes the whole window — mascot at the top, composer on the floor, instead
/// of the whole UI huddled in the bottom fifth of a tall terminal. The height
/// drops to the fixed one the moment the first message lands.
fn viewport_h_for(rows: u16, welcome: bool) -> u16 {
    let full = rows.saturating_sub(1).max(5);
    if welcome {
        full
    } else {
        full.min(INLINE_VIEWPORT_HEIGHT)
    }
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
fn rebuild_after_resize(
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
    let h = viewport_h_for(size.1, app.messages.is_empty());
    purge_and_reanchor(terminal, h)?;
    app.flushed_upto = 0;
    app.flushed_bytes = 0;
    Ok(h)
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
fn purge_and_reanchor(terminal: &mut Terminal<CrosstermBackend<Stdout>>, h: u16) -> Result<()> {
    use crossterm::cursor::MoveTo;
    use crossterm::terminal::{Clear, ClearType};
    let rows = crossterm::terminal::size()?.1;
    crossterm::execute!(
        io::stdout(),
        MoveTo(0, rows.saturating_sub(h)),
        Clear(ClearType::All),
        Clear(ClearType::Purge),
    )?;
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

async fn event_loop(
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
    let mut viewport_h = viewport_h_for(last_size.height, app.messages.is_empty());

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
            }
        }
        // One ioctl per pass keeps every width-sensitive row (composer hint,
        // status line, tool-card arg hints) honest in both render modes —
        // `last_size` above is only refreshed on the Inline path.
        app.width = terminal.backend().size()?.width.max(1);
        app.sync_input_block();
        // Leaving (or returning to) the welcome is the only time the viewport
        // height changes without the terminal resizing. Route it through the
        // same wipe + replay as /clear: the transcript is one message long at
        // that instant, so the replay is free and neither anchor artifact from
        // `viewport_h_for`'s doc comment can form.
        if app.render_mode == RenderMode::Inline {
            let want_h = viewport_h_for(last_size.height, app.messages.is_empty());
            if want_h != viewport_h {
                viewport_h = want_h;
                app.flushed_upto = 0;
                app.flushed_bytes = 0;
                app.wants_screen_wipe = true;
            }
        }
        // /clear or channel switch: the on-screen transcript no longer
        // matches the conversation — wipe screen + scrollback and re-anchor
        // so the fresh state (welcome or replayed channel) renders clean.
        if app.render_mode == RenderMode::Inline && std::mem::take(&mut app.wants_screen_wipe) {
            drop(events);
            let _ = purge_and_reanchor(terminal, viewport_h);
            events = EventStream::new();
        }
        arm_input_debounce(app, StdInstant::now());
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
        if app.needs_full_redraw {
            terminal.clear()?;
            app.needs_full_redraw = false;
        }
        terminal.draw(|f| ui::render(f, app))?;
        if app.should_quit {
            return Ok(());
        }
        // The idle welcome blinks: schedule a redraw exactly at the next blink
        // boundary, but ONLY when the mascot animates, the transcript is empty
        // (so the welcome is actually on screen), and nothing is streaming.
        // Otherwise this arm is disabled and never wakes the loop.
        let blink_live = app.mascot_mode.animated() && app.messages.is_empty() && !app.streaming;
        let blink_at = TokioInstant::from_std(app.blink.next_deadline(StdInstant::now()));
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
        tokio::select! {
            maybe = events.next() => match maybe {
                Some(Ok(ev)) => handle_event(app, ev, &tx).await,
                Some(Err(_)) | None => return Ok(()),
            },
            Some(msg) = rx.recv() => handle_stream(app, msg, &tx),
            // Never closes: PanelHandle in `app` holds a keepalive sender,
            // so this arm can't spin on a dead channel.
            Some(f) = panel_rx.recv() => match f {
                mur_common::panel::HubFrame::Insert { text } => app.set_input(&text),
            },
            _ = spinner.tick(), if app.streaming => app.tick_spinner(),
            // Wake at the blink deadline; the loop redraws at the top of the
            // next iteration, advancing the eye frame. No state change needed.
            _ = tokio::time::sleep_until(blink_at), if blink_live => {}
            _ = tokio::time::sleep_until(follow_at), if follow_armed => {
                app.poll_follow(StdInstant::now());
            }
            // Wake at the rail's next-poll deadline; the poll itself already
            // ran at the top of THIS iteration and gates on the same
            // deadline, so this arm's only job is to schedule the NEXT
            // wake-up. No state change needed here.
            _ = tokio::time::sleep_until(rail_at), if rail_armed => {}
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

async fn handle_event(app: &mut App, ev: Event, tx: &mpsc::Sender<StreamMsg>) {
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
            if app.hitl.is_some() {
                match key.code {
                    KeyCode::Char('d') if ctrl => request_quit(app, tx),
                    KeyCode::Char('c') if ctrl => decide_hitl(app, tx, false),
                    KeyCode::Char('y') | KeyCode::Char('Y') => decide_hitl(app, tx, true),
                    // [a] — always allow THIS tool name for the session
                    // (per-tool-name grain; each distinct tool still asks once).
                    KeyCode::Char('a') => {
                        if let Some(req) = &app.hitl {
                            app.session_tool_allow.insert(req.tool_name.clone());
                        }
                        decide_hitl(app, tx, true);
                    }
                    // [A] — always allow ALL tools for the session (#7 grain
                    // fix / proposal 1a): flip the global `auto_approve` so the
                    // user need not press [a] once per distinct tool name. Same
                    // session lifetime as `[a]`; cleared by `/auto off`.
                    KeyCode::Char('A') => {
                        app.auto_approve = true;
                        decide_hitl(app, tx, true);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        decide_hitl(app, tx, false)
                    }
                    KeyCode::Enter => {} // no submit while the modal is open
                    _ => {
                        app.input.input(key);
                    }
                }
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

/// Open the full-screen transcript overlay (Ctrl+O). Unlike the old
/// implementation this never drops raw mode or the alt-screen and never
/// blocks on a stdin read — it stashes the rendered text on `App` and flips
/// `overlay_open`, so the next `terminal.draw` paints it and every keypress
/// keeps flowing through the normal event loop (`overlay_key_action`
/// dispatches Esc/Enter/Ctrl+D; everything else is swallowed). That's what
/// makes Ctrl+D/Esc actually work here — the previous blocking
/// `io::stdin().read_line` outside raw mode ate Ctrl+D and turned Esc into
/// literal escape bytes that leaked into the composer.
///
/// Also saves the transcript to a temp file (best-effort) so it stays
/// reachable via the OS's native scrollback tooling even after the overlay
/// closes.
fn scrollback_dump(app: &mut App) {
    let text = dump::transcript_to_text(&app.messages);
    let path = std::env::temp_dir().join(format!("mur-transcript-{}.txt", app.agent));
    let _ = std::fs::write(&path, &text);
    app.overlay_text = Some(text);
    app.overlay_open = true;
    // Heavy overlay → borrow the alt-screen. `sync_surface` performs the
    // actual EnterAlternateScreen before the next draw.
    app.render_mode = RenderMode::Fullscreen;
    app.needs_full_redraw = true;
}

/// Write `cli.skin = name` to `~/.mur/config.yaml` atomically.
fn persist_skin(home: &std::path::Path, name: &str) -> anyhow::Result<()> {
    use mur_common::config::Config;
    let path = home.join("config.yaml");
    let mut cfg = Config::load_or_default(&path);
    cfg.cli.skin = Some(name.to_string());
    crate::store::config::save_config_at(&path, &cfg)
}

#[cfg(test)]
mod persist_skin_tests {
    use super::persist_skin;

    /// `persist_skin` must go through the shared `save_config_at` writer
    /// instead of hand-rolling its own serialise/write/rename — otherwise it
    /// inherits the bug that writer was just fixed for: a typed `Config`
    /// round-trip silently drops every top-level block it has no field for
    /// (e.g. a hand-written `research_gateway` block).
    #[test]
    fn persist_skin_preserves_blocks_the_typed_config_does_not_know() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(
            home.join("config.yaml"),
            "research_gateway:\n  brave_api_key_ref: keychain:mur/brave\n",
        )
        .unwrap();

        persist_skin(home, "dark").unwrap();

        let back = std::fs::read_to_string(home.join("config.yaml")).unwrap();
        assert!(back.contains("skin: dark"), "skin not written:\n{back}");
        assert!(back.contains("research_gateway"), "block dropped:\n{back}");
        assert!(
            back.contains("keychain:mur/brave"),
            "value dropped:\n{back}"
        );
    }
}

/// Stage a base64 image (with its mime) to send with the next message.
fn stage_image(app: &mut App, mime: &str, b64: String) {
    app.pending_image = Some((mime.to_string(), b64));
    app.push_system("📎 image attached — sent with your next message");
}

/// Grab a screenshot off the system clipboard and stage it; returns whether an
/// image was found. Used by Ctrl+V and by an empty bracketed paste (a Cmd+V of a
/// raw clipboard image on terminals that emit an empty paste event).
fn attach_clipboard_image(app: &mut App) -> bool {
    match clipboard_png() {
        Some(b64) => {
            stage_image(app, "image/png", b64);
            true
        }
        None => false,
    }
}

/// Read a PNG off the macOS clipboard and return it base64-encoded.
///
/// ponytail: macOS-only via `osascript` (zero new deps — the clipboard already
/// carries a PNG flavor). Add `arboard` + an encoder for Linux/Windows if asked.
#[cfg(target_os = "macos")]
fn clipboard_png() -> Option<String> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let tmp = std::env::temp_dir().join("mur-cli-paste.png");
    let path = tmp.to_str()?;
    // Dump the clipboard's PNG flavor to `path`; `the clipboard as «class PNGf»`
    // throws when there's no image, which the handler maps to "NOIMG".
    let script = format!(
        "try\n\
           set f to open for access (POSIX file \"{path}\") with write permission\n\
           set eof f to 0\n\
           write (the clipboard as «class PNGf») to f\n\
           close access f\n\
         on error\n\
           return \"NOIMG\"\n\
         end try"
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    if !out.status.success() || String::from_utf8_lossy(&out.stdout).contains("NOIMG") {
        return None;
    }
    let bytes = std::fs::read(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    (!bytes.is_empty()).then(|| STANDARD.encode(&bytes))
}

#[cfg(not(target_os = "macos"))]
fn clipboard_png() -> Option<String> {
    None
}

/// Recompute the completion menu from the current input. Called after every
/// edit and when Tab is pressed with the menu closed.
fn refresh_completion(app: &mut App) {
    app.completion = complete::compute(&app.input_text(), &app.skills);
}

/// Move the highlighted row by `delta`, wrapping.
fn completion_move(app: &mut App, delta: isize) {
    if let Some(c) = &mut app.completion {
        let n = c.items.len() as isize;
        if n == 0 {
            return;
        }
        c.selected = (c.selected as isize + delta).rem_euclid(n) as usize;
    }
}

/// Accept the highlighted candidate: replace the input line with its insert
/// text. A command with a subcommand layer keeps the menu open (now showing
/// layer 2); everything else closes it.
fn completion_accept(app: &mut App) {
    let Some(c) = app.completion.as_ref() else {
        return;
    };
    let Some(cand) = c.items.get(c.selected) else {
        app.completion = None;
        return;
    };
    let insert = cand.insert.clone();
    let descend = cand.has_children;
    app.set_input(&insert);
    app.completion = if descend {
        complete::compute(&app.input_text(), &app.skills)
    } else {
        None
    };
}

/// Cancel the in-flight turn (if any) on a separate connection and mark the
/// streaming bubble done locally. After this, `current_task_id` is `None`, so
/// the orphaned worker's late events are dropped by `handle_stream`.
fn cancel_in_flight(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
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
fn request_quit(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    cancel_in_flight(app, tx);
    app.should_quit = true;
}

fn handle_ctrl_c(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
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
fn decide_hitl(app: &mut App, tx: &mpsc::Sender<StreamMsg>, allow: bool) {
    decide_hitl_with_note(app, tx, allow, false);
}

fn decide_hitl_with_note(app: &mut App, tx: &mpsc::Sender<StreamMsg>, allow: bool, auto: bool) {
    if let Some(req) = app.hitl.take() {
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
        tokio::spawn(async move {
            if let Err(e) = respond_hitl(h, a, id, allow).await {
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
                if let Some(sid) = &req.step_id {
                    app.mark_card_auto_approved(sid);
                }
            }
            (true, false) => app.push_success(format!("approved `{}`", req.tool_name)),
            (false, _) => app.push_warn(format!("denied `{}`", req.tool_name)),
        }
    }
}

async fn submit(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
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
            return;
        }
    }
    // `!command` — run locally (like Claude Code's bang escape). Allowed while
    // a turn is generating: it never touches the agent connection.
    if let Some(cmd) = trimmed.strip_prefix('!').map(str::trim)
        && !cmd.is_empty()
    {
        app.clear_input();
        app.push_system(format!("running `{cmd}`…"));
        let (cmd, t) = (cmd.to_string(), tx.clone());
        tokio::spawn(async move {
            let output = stream::run_local_shell(cmd.clone()).await;
            let _ = t.send(StreamMsg::ShellDone { cmd, output }).await;
        });
        return;
    }
    // While a turn is generating: steer it if we have a live task id,
    // otherwise fall back to the old reject message.
    if app.streaming {
        if let Some(task_id) = app.current_task_id.clone() {
            let (h, a) = (app.home.clone(), app.agent.clone());
            let (msg, t) = (trimmed.clone(), tx.clone());
            app.push_system(format!("↗ steering: {trimmed}"));
            app.clear_input();
            tokio::spawn(async move {
                if let Err(e) = stream::steer_turn(h, a, task_id.clone(), msg.clone()).await {
                    let err = format!("{e:#}");
                    let out = match recover::classify_steer_failure(&err) {
                        // The runtime restarted (tasks live in memory only):
                        // the steered task is gone. Drop the dead binding and
                        // replay the user's text as a fresh turn on the same
                        // channel so it is not lost (#713).
                        recover::SteerFailure::TaskGone => StreamMsg::TurnLost {
                            task_id,
                            note: "agent restarted — continuing in this conversation".to_string(),
                            resend: Some(msg),
                        },
                        recover::SteerFailure::Other => {
                            StreamMsg::Note(format!("steer failed: {err}"))
                        }
                    };
                    let _ = t.send(out).await;
                }
            });
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
fn start_turn(app: &mut App, trimmed: String, tx: &mpsc::Sender<StreamMsg>) {
    let task_id = app.begin_user_turn(&trimmed);
    // On the first send of each session, prepend the user's working directory
    // so the agent knows which project they're in.
    let cwd_prefix = if !app.cwd_sent {
        app.cwd_sent = true;
        app.cwd
            .as_ref()
            .map(|d| format!(
                "[working directory: {path}] — pass `\"cwd\": \"{path}\"` in every bash tool call so commands run in this directory.\n\n",
                path = d.display()
            ))
            .unwrap_or_default()
    } else {
        String::new()
    };
    // Prefix any `!command` output the agent hasn't seen yet, so it has the
    // same context the user is looking at. The transcript shows only the
    // user's text; the shell blocks were already rendered when they ran.
    let outgoing = match app.take_pending_shell() {
        Some(ctx) => format!("{cwd_prefix}{ctx}\n\n{trimmed}"),
        None => format!("{cwd_prefix}{trimmed}"),
    };
    let params = build_params(
        &outgoing,
        &task_id,
        app.context_task_id.as_deref(),
        app.pending_image
            .as_ref()
            .map(|(m, b)| (m.as_str(), b.as_str())),
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

async fn handle_slash(app: &mut App, cmd: SlashCmd, tx: &mpsc::Sender<StreamMsg>) {
    match cmd {
        SlashCmd::Help => app.push_system(HELP),
        SlashCmd::Quit => request_quit(app, tx),
        SlashCmd::Clear => {
            // Stop the in-flight turn first so its worker can't write into the
            // fresh conversation after the reset.
            cancel_in_flight(app, tx);
            match Session::create(&app.home, &app.agent) {
                Ok(s) => app.start_new_session(s),
                Err(e) => app.push_system(format!("could not start new session: {e}")),
            }
        }
        SlashCmd::Sessions => match persist::list_recent(&app.home, &app.agent, RECENT_LIMIT) {
            Ok(list) if !list.is_empty() => {
                let mut out = String::from(
                    "recent conversations (resume the latest with `mur agent cli --resume`):\n",
                );
                for s in list {
                    out.push_str(&format!(
                        "  {} · {} turns · {}\n",
                        &s.id[..s.id.len().min(8)],
                        s.turns,
                        s.preview
                    ));
                }
                app.push_system(out.trim_end().to_string());
            }
            Ok(_) => app.push_system("no saved conversations yet"),
            Err(e) => app.push_system(format!("could not list sessions: {e}")),
        },
        SlashCmd::Channels { n, follow } => {
            // `--follow` never touches the current conversation: it tails
            // ANOTHER channel while this pane keeps chatting, so an in-flight
            // turn must not be cancelled for it.
            if follow {
                let recent = match persist::list_recent(&app.home, &app.agent, RECENT_LIMIT) {
                    Ok(r) => r,
                    Err(e) => {
                        app.push_system(format!("could not list channels: {e}"));
                        return;
                    }
                };
                match n {
                    None => {
                        match app.follow.take() {
                            Some(f) => app.push_system(format!("stopped following {}", f.tag())),
                            None => app.push_system(
                                "not following anything — /channels N --follow to start",
                            ),
                        }
                        return;
                    }
                    Some(n) => match recent.get(n.wrapping_sub(1)) {
                        Some(s) => {
                            let id = s.id.clone();
                            match app.start_follow(&id, StdInstant::now()) {
                                Ok(()) => app.push_system(format!(
                                    "following {} — new events appear here; /channels --follow to stop",
                                    &id[..id.len().min(8)]
                                )),
                                Err(e) => app.push_system(format!("could not follow: {e:#}")),
                            }
                        }
                        None => app.push_system(format!("no channel {n}")),
                    },
                }
                return;
            }
            // Cancel any in-flight stream before we potentially switch channels.
            if app.streaming {
                if let Some(tid) = app.current_task_id.clone() {
                    let _ = cancel_task(app.home.clone(), app.agent.clone(), tid).await;
                }
                app.finish_partial();
            }
            let recent = match persist::list_recent(&app.home, &app.agent, RECENT_LIMIT) {
                Ok(r) => r,
                Err(e) => {
                    app.push_system(format!("could not list channels: {e}"));
                    return;
                }
            };
            match n {
                Some(n) => match recent.get(n.wrapping_sub(1)) {
                    Some(s) => {
                        let id = s.id.clone();
                        match app.switch_channel(&id) {
                            Ok(()) => app.push_system(format!(
                                "switched to channel {} ({} turns)",
                                &id[..id.len().min(8)],
                                app.messages
                                    .iter()
                                    .filter(|m| matches!(m.role, Role::User | Role::Agent))
                                    .count()
                            )),
                            Err(e) => app.push_system(format!("could not switch channel: {e}")),
                        }
                    }
                    None => app.push_system(format!("no channel {n}")),
                },
                None => {
                    if recent.is_empty() {
                        app.push_system("no channels yet");
                    } else {
                        let mut out = String::from(
                            "channels (/channels N to switch · N --follow to tail):\n",
                        );
                        for (i, s) in recent.iter().enumerate() {
                            out.push_str(&format!(
                                "  {} · {} turns · {}\n",
                                i + 1,
                                s.turns,
                                s.preview
                            ));
                        }
                        app.push_system(out.trim_end().to_string());
                    }
                }
            }
        }
        SlashCmd::Card => {
            let (h, a) = (app.home.clone(), app.agent.clone());
            let res = tokio::task::spawn_blocking(move || {
                dial_method(&h, &a, "agent/card", Value::Null, DialMode::Auto)
            })
            .await;
            match res {
                Ok(Ok(card)) => app.push_system(
                    serde_json::to_string_pretty(&card).unwrap_or_else(|_| card.to_string()),
                ),
                Ok(Err(e)) => app.push_system(format!("card error: {e:#}")),
                Err(e) => app.push_system(format!("card task failed: {e}")),
            }
        }
        SlashCmd::Auto(set) => {
            app.auto_approve = set.unwrap_or(!app.auto_approve);
            if app.auto_approve {
                app.push_system(
                    "auto-approve ON — every tool call is allowed without asking (this session only; /auto off to disable)",
                );
                // A prompt may already be waiting — resolve it under the new mode.
                if app.hitl.is_some() {
                    decide_hitl_with_note(app, tx, true, true);
                }
            } else {
                app.push_system("auto-approve OFF — tool calls ask again");
            }
        }
        SlashCmd::Verbose(set) => {
            app.cards_expanded = set.unwrap_or(!app.cards_expanded);
            if app.cards_expanded {
                app.push_system(
                    "verbose ON — tool cards show full args + result (Ctrl+O for the transcript any time)",
                );
            } else {
                app.push_system("verbose OFF — tool cards collapse to a one-line summary");
            }
        }
        SlashCmd::Mcp(args) => run_manage(app, move |agent| manage::run_mcp(&agent, &args)).await,
        SlashCmd::Skill(args) => {
            run_manage(app, move |agent| manage::run_skill(&agent, &args)).await
        }
        SlashCmd::Remember(args) => {
            let msg = memory_cmds::remember(&app.home, &app.agent, &args)
                .unwrap_or_else(|e| format!("remember failed: {e}"));
            app.push_system(msg);
        }
        SlashCmd::Memories => app.push_system(memory_cmds::memories(&app.home, &app.agent)),
        SlashCmd::Forget(target) => {
            let msg = memory_cmds::forget(&app.home, &app.agent, target.as_deref())
                .unwrap_or_else(|e| format!("forget failed: {e}"));
            app.push_system(msg);
        }
        SlashCmd::Skin(name_opt) => match name_opt {
            None => {
                let current = theme::skin_name(app.theme);
                app.push_system(format!("current skin: {current} — valid: dark, light, mur"));
            }
            Some(name) => {
                if !theme::is_known_skin(&name) {
                    app.push_system(format!("unknown skin '{name}' — valid: dark, light, mur"));
                } else {
                    app.theme = theme::resolve_skin(&name);
                    app.mascot_mode =
                        welcome::resolve_mascot_mode(app.theme, std::io::stdout().is_terminal());
                    let h = app.home.clone();
                    match persist_skin(&h, &name) {
                        Ok(()) => app.push_system(format!("skin changed to {name}")),
                        Err(e) => app.push_system(format!(
                            "skin changed to {name} (could not persist: {e})"
                        )),
                    }
                }
            }
        },
        SlashCmd::Panel(args) => panel::handle_panel_command(app, &args),
        SlashCmd::Open => {
            let items = crate::open_items::collect(&app.home);
            let (visible, muted) = crate::open_items::partition(items, &app.muted_origins());
            app.open_items_fp = Some(crate::open_items::fingerprint(&visible));
            app.push_system(
                crate::open_items::render(&visible, &muted)
                    .trim()
                    .to_string(),
            );
        }
        SlashCmd::Unknown(c) => app.push_system(format!("unknown command: /{c} — try /help")),
    }
}

/// Run a blocking profile-management closure off the event loop and render
/// its outcome as a system note.
async fn run_manage<F>(app: &mut App, f: F)
where
    F: FnOnce(String) -> Result<String> + Send + 'static,
{
    let agent = app.agent.clone();
    match tokio::task::spawn_blocking(move || f(agent)).await {
        Ok(Ok(text)) => app.push_system(text),
        Ok(Err(e)) => app.push_error(format!("error: {e:#}")),
        Err(e) => app.push_error(format!("task failed: {e}")),
    }
}

/// Replay the current turn's `message/send` after a dial failure, under a
/// fresh client task id: the runtime keys tasks by it, and the old id may be
/// half-registered on the peer that just died.
fn retry_send(app: &mut App, mut params: Value, tx: &mpsc::Sender<StreamMsg>) {
    let task_id = uuid::Uuid::now_v7().to_string();
    params["task_id"] = Value::String(task_id.clone());
    app.current_task_id = Some(task_id.clone());
    app.inflight_params = Some(params.clone());
    spawn_stream(
        app.home.clone(),
        app.agent.clone(),
        params,
        task_id,
        tx.clone(),
    );
}

fn handle_stream(app: &mut App, msg: StreamMsg, tx: &mpsc::Sender<StreamMsg>) {
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
        StreamMsg::Hitl { req, .. } => {
            app.saw_hitl_this_turn = true;
            if let Some(sid) = req.step_id.clone() {
                app.mark_card_awaiting(&sid);
            }
            // Session auto-approval: `/auto`/`--auto` covers every tool; the
            // modal's [a] key covers a single tool name.
            // Read lane: `--auto-reads` auto-approves read-only bash commands.
            let read_auto = app.auto_reads
                && req.tool_name == "bash"
                && req
                    .tool_input
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(bash_class::is_readonly_bash);
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
        StreamMsg::ShellDone { cmd, output } => app.push_shell(&cmd, &output),
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
            );
        }
    }
}

// ── OS notifications ─────────────────────────────────────────────────────────

/// The macOS `osascript` line for a notification, with quotes escaped. Pure so
/// the escaping is unit-tested; the spawn is in `notify_unfocused`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn notify_script(title: &str, message: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        message.replace('\\', "\\\\").replace('"', "\\\""),
        title.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// Best-effort OS notification — fired only when the terminal is unfocused.
/// Never blocks or errors the event loop (spawn-and-ignore), and emits no
/// in-terminal bell (the TUI owns the alternate screen).
fn notify_unfocused(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("osascript")
            .args(["-e", &notify_script(title, message)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args([title, message])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, message); // no-op on other platforms
    }
}

#[cfg(test)]
mod viewport_tests {
    use super::{INLINE_VIEWPORT_HEIGHT, viewport_h_for};

    /// The welcome and the chat surface want opposite things, and only one of
    /// them is safe at full height: nothing is ever flushed to scrollback while
    /// the transcript is empty, so the mascot can own the window, but a chat
    /// viewport must leave the spare rows `insert_before` needs.
    #[test]
    fn the_welcome_owns_the_window_and_chat_leaves_headroom() {
        let rows = 60;
        assert_eq!(viewport_h_for(rows, true), rows - 1);
        assert_eq!(viewport_h_for(rows, false), INLINE_VIEWPORT_HEIGHT);
        // Short window: even the welcome keeps its spare row.
        assert_eq!(viewport_h_for(12, true), 11);
        // Absurdly short: a floor beats a zero-height viewport.
        assert_eq!(viewport_h_for(2, true), 5);
        assert_eq!(viewport_h_for(2, false), 5);
    }
}

#[cfg(test)]
mod notify_tests {
    use super::notify_script;

    #[test]
    fn notify_script_escapes_quotes() {
        let s = notify_script("rustsmith", r#"finished "the" task"#);
        assert!(s.contains("display notification"));
        // clean title → PLAIN quote delimiters
        assert!(s.contains(r#"with title "rustsmith""#));
        // embedded quotes in the message ARE escaped to \"
        assert!(s.contains(r#"finished \"the\" task"#));
        // the full message is wrapped in plain delimiters
        assert!(s.contains(r#"display notification "finished \"the\" task""#));
    }
}

// ── Non-TTY plain mode ────────────────────────────────────────────────────────

/// Pipe-safe fallback: read a line from stdin, stream the reply as plain text to
/// stdout, repeat. No ANSI, no TUI. Threads conversation context across turns.
fn run_plain(home: &Path, agent: &str, auto: bool, interactive: bool) -> Result<()> {
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::io::Write as _;
    let out2 = RefCell::new(io::stdout());
    let mut context: Option<String> = None;
    let pricing = load_pricing(home, agent);

    loop {
        if interactive {
            let _ = write!(out2.borrow_mut(), "you › ");
            let _ = out2.borrow_mut().flush();
        }
        let mut line = String::new();
        // Lock only per-read so a HITL callback (Task 3) can read stdin mid-turn.
        if io::stdin().lock().read_line(&mut line)? == 0 {
            break; // EOF (Ctrl-D / end of pipe)
        }
        let text = line.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let task_id = uuid::Uuid::now_v7().to_string();
        let params = build_params(&text, &task_id, context.as_deref(), None);
        let streamed = Cell::new(false);
        // Track step_id → name from Started events so Completed can print the name.
        let step_names: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());

        let result = crate::a2a_dial::dial_message_streaming(
            home,
            agent,
            params,
            |delta, thinking, _task_id| {
                if !thinking {
                    streamed.set(true);
                    let _ = write!(out2.borrow_mut(), "{delta}");
                    let _ = out2.borrow_mut().flush();
                }
            },
            |hitl| {
                let id = hitl
                    .get("hitl_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let tool = hitl
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                let allow = if auto {
                    eprintln!("[non-interactive: auto-approving tool-approval request (--auto)]");
                    true
                } else if interactive {
                    // Outer loop releases stdin lock between reads (Task 2), so
                    // we can safely acquire a fresh lock here to prompt the user.
                    // [a]lways approves once in v1 (no session allow-set) — intentional.
                    let mut o = io::stdout();
                    let _ = write!(o, "  tool approval: {tool} — [y]es / [a]lways / [n]o? ");
                    let _ = o.flush();
                    let mut ans = String::new();
                    let _ = io::stdin().lock().read_line(&mut ans);
                    matches!(
                        ans.trim().chars().next(),
                        Some('y') | Some('Y') | Some('a') | Some('A')
                    )
                } else {
                    eprintln!(
                        "[non-interactive: auto-denying tool-approval request (use --auto to allow)]"
                    );
                    false
                };
                let _ = dial_method(
                    home,
                    agent,
                    "tool/hitl_respond",
                    serde_json::json!({ "hitl_id": id, "allow": allow }),
                    DialMode::RequireRunning,
                );
            },
            |step| {
                use crate::a2a_dial::StepEvent;
                match step {
                    StepEvent::Started {
                        step_id,
                        name,
                        args,
                        ..
                    } => {
                        // Derive a short arg hint: prefer "command" arg, else JSON.
                        let hint_raw = args
                            .get("command")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| args.to_string());
                        let hint: String = hint_raw.chars().take(PLAIN_STEP_HINT_MAX).collect();
                        step_names
                            .borrow_mut()
                            .insert(step_id.clone(), name.clone());
                        let _ = writeln!(out2.borrow_mut(), "→ {name} {hint}");
                        let _ = out2.borrow_mut().flush();
                    }
                    StepEvent::Completed {
                        step_id,
                        ok,
                        duration_ms,
                        ..
                    } => {
                        let glyph = if ok { '✔' } else { '✗' };
                        let name = step_names
                            .borrow()
                            .get(&step_id)
                            .cloned()
                            .unwrap_or_default();
                        let _ = writeln!(out2.borrow_mut(), "{glyph} {name} · {duration_ms}ms");
                        let _ = out2.borrow_mut().flush();
                    }
                }
            },
        );
        match result {
            Ok(task) => match stream::task_outcome(&task) {
                Ok((reply, tid)) => {
                    // Fall back to the final reply if the agent didn't stream deltas.
                    if !streamed.get() && !reply.trim().is_empty() {
                        write!(out2.borrow_mut(), "{reply}")?;
                    }
                    writeln!(out2.borrow_mut())?;
                    // Usage footer: total tokens + cost (reuse footer helpers).
                    if let Some(usage) = task.get("usage") {
                        let u = footer::parse_usage(usage);
                        match footer::turn_cost(&pricing, &u) {
                            Some(c) => {
                                let _ = writeln!(
                                    out2.borrow_mut(),
                                    "  {} tok · ${c:.3}",
                                    u.input + u.output
                                );
                            }
                            None => {
                                let _ = writeln!(out2.borrow_mut(), "  {} tok", u.input + u.output);
                            }
                        }
                    }
                    out2.borrow_mut().flush()?;
                    context = tid;
                }
                Err(cause) => {
                    writeln!(out2.borrow_mut(), "\nerror: {cause}")?;
                }
            },
            Err(e) => {
                writeln!(out2.borrow_mut(), "\nerror: {e:#}")?;
            }
        }
    }
    Ok(())
}
