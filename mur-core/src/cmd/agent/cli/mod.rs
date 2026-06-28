//! `mur agent cli <name>` — interactive streaming TUI chat with an agent.
//!
//! This is a terminal front-end over the already-working A2A streaming client
//! (`crate::a2a_dial::dial_message_streaming`); it adds no protocol surface. See
//! the sibling modules: [`stream`] (blocking-dial ↔ async bridge), [`app`]
//! (state), [`ui`] (ratatui render), [`markdown`] (reply rendering), and
//! [`persist`] (JSONL session log + resume).

mod access;
mod app;
mod diff;
mod dump;
mod footer;
mod manage;
mod markdown;
mod multiplex;
mod paste;
pub mod persist;
mod render_card;
mod step;
mod stream;
mod theme;
mod ui;
mod welcome;

use std::io::{self, IsTerminal, Stdout, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant as StdInstant;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{cursor, execute};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::Instant as TokioInstant;

use self::app::{App, EscAction, Role, SlashCmd, esc_action, parse_slash};
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
/// Spinner animation cadence.
const SPINNER_MS: u64 = 90;

const HELP: &str = "commands: /help  /clear (new conversation)  /card  /sessions  /channels [N] (list/switch)  /auto [on|off]  /mcp  /skill  /exit · !cmd runs a local shell command (output shared with the agent) · keys: Enter send · Alt+Enter newline · Ctrl+V attach screenshot · Ctrl+C cancel/clear · Ctrl+D quit · PageUp/PageDown scroll (or mouse wheel)";

/// Entry point dispatched from `AgentAction::Cli`.
pub async fn cmd_cli(
    names: &[String],
    resume: bool,
    auto: bool,
    skin: Option<String>,
) -> Result<()> {
    if names.len() > 1 {
        let names = names.to_vec();
        return tokio::task::spawn_blocking(move || multiplex::run(&names, resume, auto)).await?;
    }
    let name = names.first().context("at least one agent name required")?;
    let home = super::resolve_mur_home()?;
    let agent = canonicalize_agent_name(&home, name);

    // Streaming requires a live socket; fail early with a friendly hint.
    let lock = home.join("agents").join(&agent).join("running.lock");
    if !lock.exists() {
        eprintln!(
            "Agent '{agent}' is not running. Start it first with:\n    mur agent install-service {agent}\nthen retry: mur agent cli {agent}"
        );
        return Ok(());
    }

    // If this project dir is outside the agent's filesystem grants, offer to
    // add it (explicit consent, persisted; sandbox applies it on next restart).
    access::ensure_cwd_access(&agent)?;

    // Non-TTY (piped) → plain streamed text, preserving scriptability.
    if !io::stdout().is_terminal() {
        let home2 = home.clone();
        let agent2 = agent.clone();
        return tokio::task::spawn_blocking(move || run_plain(&home2, &agent2, auto)).await?;
    }

    run_tui(home, agent, resume, auto, skin).await
}

// ── TUI mode ────────────────────────────────────────────────────────────────

/// RAII terminal restore — runs on every exit path including unwind.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            EnableFocusChange
        )
        .context("enter alternate screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture,
            DisableFocusChange,
            cursor::Show
        );
        let _ = disable_raw_mode();
    }
}

async fn run_tui(
    home: PathBuf,
    agent: String,
    resume: bool,
    auto: bool,
    skin: Option<String>,
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
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture,
            DisableFocusChange,
            cursor::Show
        );
        let _ = disable_raw_mode();
        prev_hook(info);
    }));

    let _guard = TerminalGuard::enter()?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("init terminal")?;

    let mut app = build_app(&home, &agent, resume, active_theme)?;
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
    let result = event_loop(&mut terminal, &mut app).await;

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

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<StreamMsg>(stream::STREAM_CHANNEL_CAP);
    let mut events = EventStream::new();
    let mut spinner = tokio::time::interval(Duration::from_millis(SPINNER_MS));

    loop {
        app.sync_input_block();
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
        tokio::select! {
            maybe = events.next() => match maybe {
                Some(Ok(ev)) => handle_event(app, ev, &tx).await,
                Some(Err(_)) | None => return Ok(()),
            },
            Some(msg) = rx.recv() => handle_stream(app, msg, &tx),
            _ = spinner.tick(), if app.streaming => app.tick_spinner(),
            // Wake at the blink deadline; the loop redraws at the top of the
            // next iteration, advancing the eye frame. No state change needed.
            _ = tokio::time::sleep_until(blink_at), if blink_live => {}
        }
    }
}

async fn handle_event(app: &mut App, ev: Event, tx: &mpsc::Sender<StreamMsg>) {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            // HITL prompt owns the decision keys — but Ctrl+C/Ctrl+D must stay
            // live so the user is never trapped by a stale/unanswerable modal,
            // and any other key keeps going to the composer so typed text isn't
            // silently swallowed while the modal is up.
            if app.hitl.is_some() {
                match key.code {
                    KeyCode::Char('d') if ctrl => request_quit(app, tx),
                    KeyCode::Char('c') if ctrl => decide_hitl(app, tx, false),
                    KeyCode::Char('y') | KeyCode::Char('Y') => decide_hitl(app, tx, true),
                    KeyCode::Char('a') | KeyCode::Char('A') => {
                        if let Some(req) = &app.hitl {
                            app.session_tool_allow.insert(req.tool_name.clone());
                        }
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
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            match key.code {
                KeyCode::Char('d') if ctrl => request_quit(app, tx),
                KeyCode::Char('c') if ctrl => handle_ctrl_c(app, tx),
                KeyCode::Char('v') if ctrl => {
                    if !attach_clipboard_image(app) {
                        app.push_system(
                            "no image in clipboard — copy a screenshot then Ctrl+V (or Cmd+V / drag an image file)",
                        );
                    }
                }
                KeyCode::Char('o') if ctrl => {
                    if let Err(e) = scrollback_dump(app) {
                        app.push_system(format!("scrollback view failed: {e}"));
                    }
                }
                KeyCode::PageUp => {
                    app.scroll_back = app.scroll_back.saturating_add(app.scroll_page.max(1))
                }
                KeyCode::PageDown => {
                    app.scroll_back = app.scroll_back.saturating_sub(app.scroll_page.max(1))
                }
                KeyCode::Tab => complete_slash(app),
                KeyCode::Enter if alt => {
                    app.input.insert_newline();
                }
                KeyCode::Enter => submit(app, tx).await,
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
                            if let Some(text) = app.last_sent.clone() {
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
            } else if trimmed.is_empty() && attach_clipboard_image(app) {
                // image grabbed off the clipboard
            } else {
                app.input.insert_str(text);
            }
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

/// Suspend the TUI, print the full transcript to native scrollback (so the
/// terminal's own select/copy/search work), also save it to a temp file, then
/// wait for Enter and resume. Blocking is intentional — the TUI is frozen while
/// the user reads/copies; streaming messages queue until we return.
fn scrollback_dump(app: &App) -> io::Result<()> {
    let text = dump::transcript_to_text(&app.messages);
    let path = std::env::temp_dir().join(format!("mur-transcript-{}.txt", app.agent));
    let _ = std::fs::write(&path, &text);

    // Suspend: leave alt-screen + restore the normal buffer where native copy
    // and scrollback work; drop raw mode so `read_line` is canonical.
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    disable_raw_mode()?;

    let res = (|| -> io::Result<()> {
        use std::io::Write;
        let mut out = io::stdout();
        writeln!(out, "{text}")?;
        writeln!(
            out,
            "\n─── full transcript · select/copy freely · saved to {} ───",
            path.display()
        )?;
        write!(out, "press Enter to return to chat… ")?;
        out.flush()?;
        let mut buf = String::new();
        let _ = io::stdin().read_line(&mut buf);
        Ok(())
    })();

    // Resume UNCONDITIONALLY — even if a write above failed, never leave the
    // terminal in the normal buffer with raw mode off.
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    res
}

/// Write `cli.skin = name` to `~/.mur/config.yaml` atomically.
fn persist_skin(home: &std::path::Path, name: &str) -> anyhow::Result<()> {
    use mur_common::config::Config;
    let path = home.join("config.yaml");
    let mut cfg = Config::load_or_default(&path);
    cfg.cli.skin = Some(name.to_string());
    let text = serde_yaml::to_string(&cfg).context("serialise config")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, &text).context("write config tmp")?;
    std::fs::rename(&tmp, &path).context("rename config")?;
    Ok(())
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

/// Tab completes a leading-slash command to the first matching name.
fn complete_slash(app: &mut App) {
    let cur = app.input_text();
    let trimmed = cur.trim();
    if !trimmed.starts_with('/') {
        return;
    }
    if let Some(m) = app::SLASH_COMMANDS
        .iter()
        .find(|c| c.starts_with(trimmed) && **c != trimmed)
    {
        app.set_input(m);
    }
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
        app.should_quit = true;
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
        let t = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = respond_hitl(h, a, id, allow).await {
                let _ = t
                    .send(StreamMsg::Note(format!(
                        "failed to deliver decision for `{tool}`: {e:#}"
                    )))
                    .await;
            }
        });
        app.push_system(match (allow, auto) {
            (true, true) => format!("auto-approved `{}` (session)", req.tool_name),
            (true, false) => format!("approved `{}`", req.tool_name),
            (false, _) => format!("denied `{}`", req.tool_name),
        });
    }
}

async fn submit(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    let trimmed = app.input_text().trim().to_string();
    // Allow an image-only send (caption optional) when a screenshot is staged.
    if trimmed.is_empty() && app.pending_image.is_none() {
        return;
    }

    if let Some(cmd) = parse_slash(&trimmed) {
        app.clear_input();
        handle_slash(app, cmd, tx).await;
        return;
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
                if let Err(e) = stream::steer_turn(h, a, task_id, msg).await {
                    let _ = t
                        .send(StreamMsg::Note(format!("steer failed: {e:#}")))
                        .await;
                }
            });
        } else {
            app.push_system("still generating — press Ctrl+C to cancel first");
        }
        return;
    }
    app.last_sent = Some(trimmed.clone());
    app.clear_input();

    let task_id = app.begin_user_turn(&trimmed);
    // On the first send of each session, prepend the user's working directory
    // so the agent knows which project they're in.
    let cwd_prefix = if !app.cwd_sent {
        app.cwd_sent = true;
        app.cwd
            .as_ref()
            .map(|d| format!("[working directory: {}]\n\n", d.display()))
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
        SlashCmd::Channels(n) => {
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
                        let mut out = String::from("channels (type /channels N to switch):\n");
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
        SlashCmd::Mcp(args) => run_manage(app, move |agent| manage::run_mcp(&agent, &args)).await,
        SlashCmd::Skill(args) => {
            run_manage(app, move |agent| manage::run_skill(&agent, &args)).await
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
        Ok(Err(e)) => app.push_system(format!("error: {e:#}")),
        Err(e) => app.push_system(format!("task failed: {e}")),
    }
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
    match msg {
        StreamMsg::Delta { text, thinking, .. } => app.append_delta(&text, thinking),
        StreamMsg::Hitl { req, .. } => {
            app.saw_hitl_this_turn = true;
            if let Some(sid) = req.step_id.clone() {
                app.mark_card_awaiting(&sid);
            }
            // Session auto-approval: `/auto`/`--auto` covers every tool; the
            // modal's [a] key covers a single tool name.
            let auto = app.auto_approve || app.session_tool_allow.contains(&req.tool_name);
            if !app.focused && !auto {
                notify_unfocused(
                    &app.agent,
                    &format!("Tool approval needed: {}", req.tool_name),
                );
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
        }
        StreamMsg::Err { error, .. } => {
            if !app.focused {
                notify_unfocused(&app.agent, "Turn failed");
            }
            app.fail_turn(&error);
        }
        StreamMsg::Note(text) => app.push_system(text),
        StreamMsg::ShellDone { cmd, output } => app.push_shell(&cmd, &output),
        StreamMsg::StepStarted {
            step_id,
            name,
            args,
            ..
        } => {
            app.saw_step_this_turn = true;
            app.push_step_started(step_id, name, args);
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
fn run_plain(home: &Path, agent: &str, auto: bool) -> Result<()> {
    use std::cell::Cell;
    use std::io::BufRead;
    let stdin = io::stdin();
    let mut out = io::stdout();
    let mut context: Option<String> = None;

    for line in stdin.lock().lines() {
        let line = line?;
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let task_id = uuid::Uuid::now_v7().to_string();
        let params = build_params(text, &task_id, context.as_deref(), None);
        let streamed = Cell::new(false);
        let result = crate::a2a_dial::dial_message_streaming(
            home,
            agent,
            params,
            |delta, thinking, _task_id| {
                if !thinking {
                    streamed.set(true);
                    let _ = write!(out, "{delta}");
                    let _ = out.flush();
                }
            },
            |hitl| {
                // No TTY to prompt on: resolve immediately (on a separate
                // connection) so the turn doesn't block for the full HITL
                // timeout — allow under --auto, deny otherwise — and tell the
                // user on stderr.
                let id = hitl
                    .get("hitl_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if auto {
                    eprintln!("[non-interactive: auto-approving tool-approval request (--auto)]");
                } else {
                    eprintln!(
                        "[non-interactive: auto-denying tool-approval request (use --auto to allow)]"
                    );
                }
                let _ = dial_method(
                    home,
                    agent,
                    "tool/hitl_respond",
                    serde_json::json!({ "hitl_id": id, "allow": auto }),
                    DialMode::RequireRunning,
                );
            },
            |_step| {},
        );
        match result {
            Ok(task) => match stream::task_outcome(&task) {
                Ok((reply, tid)) => {
                    // Fall back to the final reply if the agent didn't stream deltas.
                    if !streamed.get() && !reply.trim().is_empty() {
                        write!(out, "{reply}")?;
                    }
                    writeln!(out)?;
                    out.flush()?;
                    context = tid;
                }
                Err(cause) => {
                    writeln!(out, "\nerror: {cause}")?;
                }
            },
            Err(e) => {
                writeln!(out, "\nerror: {e:#}")?;
            }
        }
    }
    Ok(())
}
