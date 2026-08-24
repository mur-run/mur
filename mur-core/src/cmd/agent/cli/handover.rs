//! Hand the terminal to an interactive child, then take it back.
//!
//! murmur runs on the MAIN screen with a bottom-anchored `Viewport::Inline`
//! (see `TerminalGuard::enter`), so the child's output lands naturally in
//! scrollback above the viewport — no alternate screen is involved.
//!
//! Three things here are load-bearing and easy to get wrong:
//!
//! * The caller MUST drop the `EventStream` first. It owns stdin, and a child
//!   inheriting stdin would race murmur for the user's keystrokes — the paste
//!   prompt in an OAuth flow would eat characters.
//! * Re-entry happens in `Drop`, so a child that panics, exits non-zero, or is
//!   killed still leaves a usable terminal.
//! * Re-anchoring must NOT purge. `purge_and_reanchor` clears scrollback,
//!   which would erase the login transcript — including the URL the user still
//!   needs and any failure message.

use anyhow::{Context, Result};
use crossterm::cursor;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange,
};
use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, Stdout, Write};
use std::process::ExitStatus;
use std::sync::atomic::Ordering;

use super::app::HandoverRequest;
use super::{
    ON_ALT, keyboard_enhancement_active, pop_keyboard_enhancement, push_keyboard_enhancement,
};

/// How many times to retry building the replacement terminal. The
/// cursor-position query inside `Terminal::with_options` needs crossterm's
/// internal event reader, and the just-dropped `EventStream`'s background
/// thread can hold that lock a beat longer — the same race
/// `purge_and_reanchor` already guards against.
const REANCHOR_RETRIES: usize = 20;

pub fn split_argv(req: &HandoverRequest) -> Option<(&str, &[String])> {
    let (first, rest) = req.argv.split_first()?;
    Some((first.as_str(), rest))
}

/// Restores raw mode and the terminal's mode set on drop, so an early return
/// or a panic inside the handover cannot leave the user in a broken shell.
///
/// **This must reverse everything `TerminalGuard::enter` set, not just raw
/// mode.** Two of the four are easy to miss and both bite the login flow
/// specifically:
///
/// * **Keyboard enhancement.** murmur pushes
///   `DISAMBIGUATE_ESCAPE_CODES` when the terminal supports it. Leaving it
///   pushed means the child reads a different escape-sequence dialect than it
///   expects — and the child here is an OAuth flow where the user pastes a
///   code, which is the worst possible place for mangled input.
/// * **The alternate screen.** `sync_surface` puts murmur on the alt-screen for
///   heavy overlays (Ctrl+O, `/mcp`, `/skill`). If `/login` is invoked while
///   one is open, a child that draws there has its output discarded the moment
///   the screen is left — including the authorisation URL the user needs.
///
/// `pop_keyboard_enhancement` and `ON_ALT` live in `cli/mod.rs` and are
/// `pub(super)` so they can be reused here rather than duplicated — two
/// sources of truth for "is the alt-screen on" is how the panic hook and this
/// guard would start disagreeing.
struct Suspended {
    /// Whether the alt-screen was on when we suspended, so resume can restore
    /// it. Read from `ON_ALT`, not re-derived.
    was_alt: bool,
    /// Whether keyboard enhancement was active, so resume can re-push it.
    kbd_enhanced: bool,
}

impl Suspended {
    /// Give the terminal back to the shell. Mirrors `TerminalGuard::drop`.
    fn begin(viewport_h: u16) -> Result<Self> {
        let kbd_enhanced = keyboard_enhancement_active();
        pop_keyboard_enhancement(kbd_enhanced);
        let was_alt = ON_ALT.swap(false, Ordering::Relaxed);
        if was_alt {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        // Move below the inline viewport so the child does not paint over it,
        // then clear only the visible rows from there down. FromCursorDown
        // does not touch scrollback.
        let rows = crossterm::terminal::size()?.1;
        execute!(
            io::stdout(),
            cursor::MoveTo(0, rows.saturating_sub(viewport_h)),
            Clear(ClearType::FromCursorDown),
            DisableBracketedPaste,
            DisableFocusChange,
            cursor::Show,
        )
        .context("release terminal modes")?;
        disable_raw_mode().context("disable raw mode")?;
        Ok(Self {
            was_alt,
            kbd_enhanced,
        })
    }
}

impl Drop for Suspended {
    fn drop(&mut self) {
        let _ = enable_raw_mode();
        let _ = execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange);
        if self.was_alt {
            let _ = execute!(io::stdout(), EnterAlternateScreen);
            ON_ALT.store(true, Ordering::Relaxed);
        }
        push_keyboard_enhancement(self.kbd_enhanced);
    }
}

/// Run `req` with the terminal handed over. The caller must have dropped the
/// `EventStream` and must recreate it afterwards.
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    viewport_h: u16,
    req: &HandoverRequest,
) -> Result<ExitStatus> {
    let (prog, args) = split_argv(req).context("handover requested with empty argv")?;

    let status = {
        let _suspended = Suspended::begin(viewport_h)?;
        // Inherit all three handles: this is an interactive flow.
        std::process::Command::new(prog)
            .args(args)
            .status()
            .with_context(|| format!("run {prog}"))?
        // `_suspended` drops here — raw mode is back on before we redraw,
        // and stays restored even if `status()` returned Err above.
    };

    // Make room below whatever the child printed, then anchor a fresh inline
    // viewport there. No Clear(All), no Clear(Purge): the login transcript
    // stays in scrollback where the user can still read it.
    let mut out = io::stdout();
    for _ in 0..viewport_h {
        writeln!(out)?;
    }
    out.flush()?;

    let rows = crossterm::terminal::size()?.1;
    execute!(out, cursor::MoveTo(0, rows.saturating_sub(viewport_h)))?;

    let mut last_err = None;
    for _ in 0..REANCHOR_RETRIES {
        match Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Inline(viewport_h),
            },
        ) {
            Ok(t) => {
                *terminal = t;
                return Ok(status);
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not re-anchor the viewport after handover: {}",
        last_err.map_or_else(|| "unknown".to_string(), |e| e.to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_argv_is_split_into_program_and_args() {
        let req = crate::cmd::agent::cli::app::HandoverRequest {
            argv: vec!["claude".into(), "auth".into(), "login".into()],
            label: "Anthropic".into(),
        };
        let (prog, args) = split_argv(&req).expect("non-empty argv");
        assert_eq!(prog, "claude");
        assert_eq!(args, ["auth", "login"]);
    }

    #[test]
    fn empty_argv_is_rejected_rather_than_spawning_a_shell() {
        let req = crate::cmd::agent::cli::app::HandoverRequest {
            argv: vec![],
            label: "x".into(),
        };
        assert!(split_argv(&req).is_none());
    }
}
