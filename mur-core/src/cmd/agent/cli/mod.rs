//! `mur agent cli <name>` — interactive streaming TUI chat with an agent.
//!
//! This is a terminal front-end over the already-working A2A streaming client
//! (`crate::a2a_dial::dial_message_streaming`); it adds no protocol surface. See
//! the sibling modules: [`stream`] (blocking-dial ↔ async bridge), [`app`]
//! (state), [`ui`] (ratatui render), [`markdown`] (reply rendering), and
//! [`persist`] (JSONL session log + resume).

mod app;
mod markdown;
mod persist;
mod stream;
mod ui;

use std::io::{self, IsTerminal, Stdout, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers,
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

use self::app::{App, SlashCmd, parse_slash};
use self::persist::Session;
use self::stream::{StreamMsg, build_params, cancel_task, respond_hitl, spawn_stream};
use crate::a2a_dial::{DialMode, canonicalize_agent_name, dial_method};

/// How many transcript lines PageUp/PageDown scroll.
const SCROLL_STEP: u16 = 5;
/// Spinner animation cadence.
const SPINNER_MS: u64 = 90;

const HELP: &str = "commands: /help  /clear (new conversation)  /card  /sessions  /auto [on|off]  /exit · !cmd runs a local shell command (output shared with the agent) · keys: Enter send · Alt+Enter newline · Ctrl+C cancel/clear · Ctrl+D quit · PageUp/PageDown scroll";

/// Entry point dispatched from `AgentAction::Cli`.
pub async fn cmd_cli(name: &str, resume: bool, auto: bool) -> Result<()> {
    let home = super::resolve_mur_home()?;
    let agent = canonicalize_agent_name(&home, name);

    // Streaming requires a live socket; fail early with a friendly hint.
    let lock = home.join("agents").join(&agent).join("running.lock");
    if !lock.exists() {
        eprintln!(
            "Agent '{agent}' is not running. Start it first, e.g.:\n    mur agent run {agent}\nthen retry: mur agent cli {agent}"
        );
        return Ok(());
    }

    // Non-TTY (piped) → plain streamed text, preserving scriptability.
    if !io::stdout().is_terminal() {
        let home2 = home.clone();
        let agent2 = agent.clone();
        return tokio::task::spawn_blocking(move || run_plain(&home2, &agent2, auto)).await?;
    }

    run_tui(home, agent, resume, auto).await
}

// ── TUI mode ────────────────────────────────────────────────────────────────

/// RAII terminal restore — runs on every exit path including unwind.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)
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
            cursor::Show
        );
        let _ = disable_raw_mode();
    }
}

async fn run_tui(home: PathBuf, agent: String, resume: bool, auto: bool) -> Result<()> {
    // Restore the terminal even if a later panic unwinds past the guard.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            cursor::Show
        );
        let _ = disable_raw_mode();
        prev_hook(info);
    }));

    let _guard = TerminalGuard::enter()?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("init terminal")?;

    let mut app = build_app(&home, &agent, resume)?;
    if auto {
        app.auto_approve = true;
        app.push_system("auto-approve is ON for this session (--auto) — every tool call will be allowed without asking");
    }
    let result = event_loop(&mut terminal, &mut app).await;

    drop(_guard);
    let _ = terminal.show_cursor();
    result
}

fn build_app(home: &Path, agent: &str, resume: bool) -> Result<App> {
    if resume {
        if let Some(info) = persist::latest(home, agent)? {
            let turns = persist::load(&info.path)?;
            let mut app = App::new(
                home.to_path_buf(),
                agent.to_string(),
                Session::from_path(info.path),
            );
            app.load_history(turns);
            app.push_system(format!(
                "resumed conversation ({} turns) — {HELP}",
                app.messages.len()
            ));
            return Ok(app);
        }
        let mut app = App::new(
            home.to_path_buf(),
            agent.to_string(),
            Session::create(home, agent)?,
        );
        app.push_system(format!(
            "no saved conversation to resume; starting fresh. {HELP}"
        ));
        return Ok(app);
    }
    let mut app = App::new(
        home.to_path_buf(),
        agent.to_string(),
        Session::create(home, agent)?,
    );
    app.push_system(HELP);
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
        terminal.draw(|f| ui::render(f, app))?;
        if app.should_quit {
            return Ok(());
        }
        tokio::select! {
            maybe = events.next() => match maybe {
                Some(Ok(ev)) => handle_event(app, ev, &tx).await,
                Some(Err(_)) | None => return Ok(()),
            },
            Some(msg) = rx.recv() => handle_stream(app, msg, &tx),
            _ = spinner.tick(), if app.streaming => app.tick_spinner(),
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
            let alt = key.modifiers.contains(KeyModifiers::ALT);
            match key.code {
                KeyCode::Char('d') if ctrl => request_quit(app, tx),
                KeyCode::Char('c') if ctrl => handle_ctrl_c(app, tx),
                KeyCode::PageUp => app.scroll_back = app.scroll_back.saturating_add(SCROLL_STEP),
                KeyCode::PageDown => app.scroll_back = app.scroll_back.saturating_sub(SCROLL_STEP),
                KeyCode::Tab => complete_slash(app),
                KeyCode::Enter if alt => {
                    app.input.insert_newline();
                }
                KeyCode::Enter => submit(app, tx).await,
                _ => {
                    app.input.input(key);
                }
            }
        }
        Event::Paste(text) => {
            app.input.insert_str(text);
        }
        _ => {}
    }
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
    if trimmed.is_empty() {
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
    // Reject (but DON'T discard) a message typed while a turn is generating —
    // clearing the input here would silently lose what the user composed.
    if app.streaming {
        app.push_system("still generating — press Ctrl+C to cancel first");
        return;
    }
    app.clear_input();

    let task_id = app.begin_user_turn(&trimmed);
    // Prefix any `!command` output the agent hasn't seen yet, so it has the
    // same context the user is looking at. The transcript shows only the
    // user's text; the shell blocks were already rendered when they ran.
    let outgoing = match app.take_pending_shell() {
        Some(ctx) => format!("{ctx}\n\n{trimmed}"),
        None => trimmed.clone(),
    };
    let params = build_params(&outgoing, &task_id, app.context_task_id.as_deref());
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
        SlashCmd::Sessions => {
            match persist::list_recent(&app.home, &app.agent, persist::RECENT_LIMIT) {
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
        SlashCmd::Unknown(c) => app.push_system(format!("unknown command: /{c} — try /help")),
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
            // Session auto-approval: `/auto`/`--auto` covers every tool; the
            // modal's [a] key covers a single tool name.
            let auto = app.auto_approve || app.session_tool_allow.contains(&req.tool_name);
            app.hitl = Some(req);
            if auto {
                decide_hitl_with_note(app, tx, true, true);
            }
        }
        StreamMsg::Done { task, .. } => match stream::task_outcome(&task) {
            Ok((reply, task_id)) => app.finish_agent_turn(reply, task_id),
            Err(cause) => app.fail_turn(&cause),
        },
        StreamMsg::Err { error, .. } => app.fail_turn(&error),
        StreamMsg::Note(text) => app.push_system(text),
        StreamMsg::ShellDone { cmd, output } => app.push_shell(&cmd, &output),
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
        let params = build_params(text, &task_id, context.as_deref());
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
                    eprintln!("[non-interactive: auto-denying tool-approval request (use --auto to allow)]");
                }
                let _ = dial_method(
                    home,
                    agent,
                    "tool/hitl_respond",
                    serde_json::json!({ "hitl_id": id, "allow": auto }),
                    DialMode::RequireRunning,
                );
            },
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
