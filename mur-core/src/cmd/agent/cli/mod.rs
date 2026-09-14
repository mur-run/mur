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
mod deep_research;
mod diff;
mod dump;
mod events;
mod fleet_rail;
mod follow;
mod footer;
mod handover;
mod hitl;
mod input;
mod login;
mod manage;
mod markdown;
pub mod memory_cmds;
mod model_cmd;
mod multiplex;
mod notify;
mod panel;
mod paste;
pub mod persist;
mod plain;
mod recover;
mod render_card;
mod secret_cmd;
mod settlement;
mod shell;
mod shell_complete;
mod slash_cmds;
mod step;
mod stream;
mod stream_handler;
mod suggest;
mod term;
mod theme;
mod turn;
mod ui;
mod welcome;

// The nine modules carved out of this file keep the visibility they had
// when they were in it: private to `cli`, reachable from every descendant.
// `pub(super)` gives the visibility, these re-exports give the path.
use events::*;
use hitl::*;
use input::*;
use notify::*;
use plain::*;
use slash_cmds::*;
use stream_handler::*;
use term::*;
use turn::*;

#[cfg(test)]
mod tests;

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
use self::shell::{ShellRoute, route_shell_output, shell_block};
use self::stream::{StreamMsg, build_params, cancel_task, respond_hitl, spawn_stream};
use crate::a2a_dial::{DialMode, canonicalize_agent_name, dial_method};

/// Every model the registry knows how to price, plus which one this agent is
/// configured to use.
///
/// Pricing used to be resolved once at startup from `profile.model_ref` and
/// applied to the whole session. That is only correct while the agent actually
/// runs the model it was configured with — and the runtime's fallback chain
/// substitutes a different one whenever a candidate is unreachable, out of
/// credit, or serving a model id the provider has retired. The footer went on
/// charging at the configured model's rates, so the `$x est` figure was
/// confidently wrong and nothing said a substitution had happened. The runtime
/// already reports which model answered; this book is what lets the footer use
/// it (#947).
pub(crate) struct PricingBook {
    /// Registry key → pricing, for every entry.
    by_key: std::collections::HashMap<String, footer::Pricing>,
    /// Provider-side model name → registry key. Populated from each entry's
    /// `model:` field.
    key_of_model: std::collections::HashMap<String, String>,
    /// The key this agent's profile points at, when it uses one.
    pub configured_key: Option<String>,
}

impl PricingBook {
    pub fn pricing_for_key(&self, key: &str) -> footer::Pricing {
        self.by_key.get(key).cloned().unwrap_or_default()
    }

    /// Registry key for a provider-reported model name.
    ///
    /// Exact match first, then the LONGEST registry model id that prefixes the
    /// reported name — providers append build dates (`claude-haiku-4-5` is
    /// reported as `claude-haiku-4-5-20251001`). Longest-first matters: a
    /// shorter, older id would otherwise shadow a newer one that shares its
    /// prefix.
    pub fn key_for_model(&self, model: &str) -> Option<&str> {
        if let Some(k) = self.key_of_model.get(model) {
            return Some(k.as_str());
        }
        self.key_of_model
            .iter()
            .filter(|(name, _)| !name.is_empty() && model.starts_with(name.as_str()))
            .max_by_key(|(name, _)| name.len())
            .map(|(_, k)| k.as_str())
    }
}

/// Build the pricing book and resolve the agent's configured key. Falls back to
/// an empty book (every lookup → unknown pricing) on any read error.
fn load_pricing(home: &std::path::Path, agent: &str) -> (footer::Pricing, PricingBook) {
    let mut book = PricingBook {
        by_key: Default::default(),
        key_of_model: Default::default(),
        configured_key: None,
    };
    let Ok(reg) = mur_common::model::ModelRegistry::load_from(&home.join("models.yaml")) else {
        return (footer::Pricing::default(), book);
    };
    for (key, entry) in &reg.models {
        let (input, output) = entry.effective_costs();
        book.by_key.insert(
            key.clone(),
            footer::Pricing {
                in_per_1k: input,
                out_per_1k: output,
                window: entry.context_window,
            },
        );
        book.key_of_model.insert(entry.model.clone(), key.clone());
    }
    book.configured_key = crate::cmd::agent::load_profile_for_edit(agent)
        .ok()
        .and_then(|(_, p)| p.model_ref);
    let current = book
        .configured_key
        .as_deref()
        .map(|k| book.pricing_for_key(k))
        .unwrap_or_default();
    (current, book)
}

/// How many recent conversations `/sessions` lists.
const RECENT_LIMIT: usize = 10;
/// Mouse wheel scrolls one line per event (trackpads fire 10-20 events/sec, so
/// per-line granularity stays smooth); PageUp/PageDown move a full screenful.
const MOUSE_SCROLL_STEP: u16 = 1;
/// Fixed height of the Inline-mode viewport: 8 (max composer lines) + 2
/// (composer border) + 1 (status line) + 9 (tail preview of the
/// currently-streaming reply; the band draws no border of its own). Generous
/// enough that the common case (short composer, short-to-medium reply)
/// never scrolls within its own area; a very long streaming reply just
/// shows its latest lines until it finishes and flushes to scrollback.
const INLINE_VIEWPORT_HEIGHT: u16 = 20;
/// Spinner animation cadence.
const SPINNER_MS: u64 = 90;
/// Max chars of an arg hint shown on a step line in `--plain` mode.
const PLAIN_STEP_HINT_MAX: usize = 120;

/// Tell the running agent its memory set changed.
///
/// `/remember` and `/forget` write to disk from the CLI process; the agent
/// serves a snapshot, so without this dial the change only took effect on the
/// next restart. Same `RuntimeSkills::reload` the in-process `remember` tool
/// calls — one mechanism, two triggers.
///
/// Returns a suffix for the confirmation line. A stopped agent, or one built
/// before `memory/reload` existed, is not an error: the write already landed
/// and its next start picks it up.
async fn push_memory_reload(home: &std::path::Path, agent: &str) -> String {
    let (h, ag) = (home.to_path_buf(), agent.to_string());
    let res = tokio::task::spawn_blocking(move || {
        dial_method(
            &h,
            &ag,
            "memory/reload",
            serde_json::json!({}),
            DialMode::Auto,
        )
    })
    .await;
    match res {
        Ok(Ok(_)) => String::new(),
        _ => " (the running agent will pick this up on its next start)".into(),
    }
}

/// The `/help` cheatsheet: one row per group, the settings row first among
/// the things a session changes. Built rather than a literal so the skin
/// list is the one `/skin` accepts — a hand-written copy said `dark` for a
/// year after `ansi` became the name.
fn help_text() -> String {
    let skins = theme::SKIN_NAMES.replace(", ", "|");
    let settings = format!(
        "  settings  /model [N|name] · /effort [level] · /skin [{skins}] · /auto [on|off] · /verbose [on|off] (expand tool cards)"
    );
    [
        "commands",
        "  chat      /clear (new conversation) · /sessions · /channels [N] (list/switch) · /channels N --follow (live-tail; bare --follow stops)",
        "  look      /card · /open (outstanding items) · /memories",
        settings.as_str(),
        "  agent     /mcp · /skill · /secret <KEY> [--delete] (hidden input, never enters the chat) · /login [anthropic|chatgpt] (OAuth health; not `mur auth login`)",
        "  memory    /remember <text> · /forget <name|last>",
        "  research  /deep-research [question|status|stop|setup]  run the research fleet (/research)",
        "  more      /panel [tab] (Hub companion window) · /help · /quit (or /exit)",
        "  !cmd      run a local shell command; its output is sent to the agent as your message · Tab completes commands and paths",
        "keys        Enter send · Shift+Enter newline · Ctrl+V image · Ctrl+O transcript · Ctrl+C cancel/clear · Ctrl+D quit · PageUp/PageDown scroll",
        "menus       ↑↓ move · Tab accept · Esc close",
    ]
    .join("\n")
}

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
        if fleet.is_some() {
            eprintln!(
                "note: --fleet is only shown in the interactive TUI; it is ignored in plain/piped mode."
            );
        }
        let home2 = home.clone();
        let agent2 = agent.clone();
        let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
        return tokio::task::spawn_blocking(move || {
            run_plain(&home2, &agent2, auto, auto_reads, interactive)
        })
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

pub(super) fn push_keyboard_enhancement(supported: bool) {
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
// `TerminalGuard`/panic hook/`scrollback_dump`/`handover::Suspended`) purely
// so *they* don't have to re-query the terminal — but the actual decision to
// pop is driven by `KB_ENHANCEMENT_ACTIVE`, not by that flag. Deliberately
// does NOT call `supports_keyboard_enhancement()` itself: that sends a second
// terminal query-and-wait (up to crossterm's 2s timeout). If the reply
// arrives after we've already disabled raw mode / left the alternate screen,
// nothing reads it — the raw escape bytes fall through to the shell's
// cooked-mode stdin and get echoed as garbage (e.g.
// `^[[?1u^[[?62;22;52c`). We already know from the push above whether the
// protocol is actually active, so just use that.
pub(super) fn pop_keyboard_enhancement(_supported: bool) {
    if KB_ENHANCEMENT_ACTIVE.swap(false, Ordering::Relaxed) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
}

/// A pure read of whether keyboard enhancement is currently pushed — unlike
/// `pop_keyboard_enhancement`, this does not clear `KB_ENHANCEMENT_ACTIVE`.
/// `handover::Suspended::begin` needs the state remembered for a later
/// re-push on resume, so it peeks here first and lets `pop_keyboard_enhancement`
/// do the actual (destructive) pop right after — a read and a mutation kept as
/// two honestly-named steps instead of one function trying to do both.
pub(super) fn keyboard_enhancement_active() -> bool {
    KB_ENHANCEMENT_ACTIVE.load(Ordering::Relaxed)
}

/// Tracks whether the physical terminal is currently on the alternate screen.
/// Owned here (not inside `sync_surface`) so the RAII guard's `Drop`, the
/// panic hook, and the terminal-handover guard (`handover::Suspended`) can all
/// decide whether a `LeaveAlternateScreen`/`EnterAlternateScreen` is actually
/// needed — Inline mode never entered the alt-screen, so leaving it would
/// corrupt the user's scrollback on exit. `sync_surface` and
/// `handover::Suspended` are the only writers (the latter only for the
/// duration of a handover, always restoring what it found); the guard/panic
/// paths only read.
pub(super) static ON_ALT: AtomicBool = AtomicBool::new(false);

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

// ── Non-TTY plain mode ────────────────────────────────────────────────────────
