//! Hand the terminal to an interactive child, then take it back.
//!
//! murmur runs on the MAIN screen with a bottom-anchored `Viewport::Inline`
//! (see `TerminalGuard::enter`), so the child's output lands naturally in
//! scrollback above the viewport — no alternate screen is involved.
//!
//! Four things here are load-bearing and easy to get wrong:
//!
//! * The caller MUST drop the `EventStream` first. It owns stdin, and a child
//!   inheriting stdin would race murmur for the user's keystrokes — the paste
//!   prompt in an OAuth flow would eat characters.
//! * Re-entry happens in `Drop`, so a child that panics, exits non-zero, or is
//!   killed still leaves a usable terminal.
//! * Re-anchoring must NOT purge. `purge_and_reanchor` clears scrollback,
//!   which would erase the login transcript — including the URL the user still
//!   needs and any failure message.
//! * Ctrl-C must reach the child, not murmur. `begin` disables raw mode, so
//!   Ctrl-C becomes a real SIGINT to the whole foreground process group —
//!   which includes murmur, since nothing here calls `setpgid`. Unix ignores
//!   it in the parent and resets it to default in the child (`pre_exec`, see
//!   `Suspended::begin`/`Drop` and `run`); Windows has neither mechanism, so
//!   Ctrl-C during a handover still kills murmur there.

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
#[cfg(unix)]
use std::os::unix::process::CommandExt;
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
///   heavy overlays (Ctrl+O, `/mcp`, `/skill`). A child that draws there has
///   its output discarded the moment the screen is left — including the
///   authorisation URL the user needs. Today `/login` cannot actually be
///   reached from an overlay (`RenderMode::Fullscreen` is set only by
///   `scrollback_dump`, and the overlay owns every keypress while it is open),
///   so this is defence in depth against a future overlay that does accept
///   slash input, not a path with a live caller — but it is `ON_ALT` that
///   decides, not an assumption about which overlays exist.
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
    /// SIGINT's disposition before `begin` ignored it, so `Drop` restores
    /// exactly that rather than assuming SIG_DFL. Unix only — Windows has no
    /// `pre_exec`, so the child shares murmur's console and Ctrl-C during a
    /// handover still kills murmur there (see the module doc comment).
    #[cfg(unix)]
    old_sigint: libc::sighandler_t,
}

impl Suspended {
    /// Give the terminal back to the shell. Mirrors `TerminalGuard::drop`.
    ///
    /// Captures everything `Drop` needs and constructs `Self` FIRST, before
    /// any of the fallible or mutating work below runs. `Self` existing (even
    /// just bound to a local) is what makes `Drop` fire on an early `?`
    /// return — construct-after-mutate would leave keyboard enhancement
    /// popped, `ON_ALT` cleared, and SIGINT ignored with no guard left to
    /// undo any of it for the rest of the session. Every individual restore
    /// in `Drop` is idempotent (see its doc comment), so constructing early
    /// and mutating after is safe even though `Drop` may then "restore"
    /// something this function never got around to changing.
    fn begin(viewport_h: u16) -> Result<Self> {
        let kbd_enhanced = keyboard_enhancement_active();
        let was_alt = ON_ALT.load(Ordering::Relaxed);
        // Ignore SIGINT here in the parent so a Ctrl-C during the child's
        // lifetime doesn't also terminate murmur — nothing calls `setpgid`,
        // so parent and child share a process group and the terminal signals
        // both (see the module doc comment; `run` resets this to default in
        // the child before exec). `libc::signal` atomically reads the old
        // disposition and installs the new one, so this doubles as the
        // capture for `Drop`. SIG_IGN can only fail on an invalid or
        // uncatchable signal number, neither of which applies to SIGINT —
        // practically infallible, same class of call as the `ON_ALT.load`
        // above it.
        #[cfg(unix)]
        let old_sigint = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
        let suspended = Self {
            was_alt,
            kbd_enhanced,
            #[cfg(unix)]
            old_sigint,
        };

        pop_keyboard_enhancement(kbd_enhanced);
        ON_ALT.store(false, Ordering::Relaxed);
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
        Ok(suspended)
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
        // Put SIGINT's disposition back to whatever it was before `begin`,
        // rather than assuming SIG_DFL — a defensive default in case
        // something upstream had already customized it.
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGINT, self.old_sigint);
        }
    }
}

/// Run `req` with the terminal handed over. The caller must have dropped the
/// `EventStream` and must recreate it afterwards.
///
/// On Unix, Ctrl-C during the child's lifetime is ignored by murmur and reset
/// to default in the child before exec, so it reaches only the child (see the
/// module doc comment and `Suspended::begin`). This crate installs no such
/// guard on Windows, so Ctrl-C there kills murmur along with the child.
pub fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    viewport_h: u16,
    req: &HandoverRequest,
) -> Result<ExitStatus> {
    let (prog, args) = split_argv(req).context("handover requested with empty argv")?;

    let status = {
        let _suspended = Suspended::begin(viewport_h)?;
        // Inherit all three handles: this is an interactive flow.
        let mut cmd = std::process::Command::new(prog);
        cmd.args(args);
        // `begin` ignores SIGINT in the parent (see its doc comment); SIG_IGN
        // is inherited across exec, so without this the child would silently
        // ignore Ctrl-C too. Reset to the default disposition right before
        // exec so the child reacts to Ctrl-C normally. Unix only: Windows has
        // no `pre_exec` — see the module doc comment.
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                Ok(())
            });
        }
        cmd.status().with_context(|| format!("run {prog}"))?
        // `_suspended` drops here — raw mode is back on before we redraw,
        // and stays restored even if `status()` returned Err above.
    };

    reanchor(terminal, viewport_h)?;
    Ok(status)
}

/// Anchor a fresh Inline viewport of height `h` at the bottom of the screen
/// **without purging scrollback**.
///
/// This is the non-purging counterpart to `cli::purge_and_reanchor`, and the
/// difference is the whole point of the module: `Clear(ClearType::Purge)`
/// would erase the login transcript — the URL the user still needs, the code
/// prompt, any failure message.
///
/// Two callers, and they must stay the same code:
///
/// * `run`, above, once the child has exited; and
/// * the main loop, *before* the handover, when the notice it just pushed
///   changed the viewport height (welcome height → chat height). Updating only
///   the loop's local `viewport_h` there would leave `terminal` anchored where
///   it was, so the draw would land on one geometry and `Suspended::begin`
///   would clear another.
///
/// The leading newlines make room: after the child, they push its output up so
/// the viewport does not land on top of it; before it, they do the same for
/// whatever murmur had already painted.
pub(super) fn reanchor(terminal: &mut Terminal<CrosstermBackend<Stdout>>, h: u16) -> Result<()> {
    let mut out = io::stdout();
    for _ in 0..h {
        writeln!(out)?;
    }
    out.flush()?;

    let rows = crossterm::terminal::size()?.1;
    execute!(out, cursor::MoveTo(0, rows.saturating_sub(h)))?;

    let mut last_err = None;
    for _ in 0..REANCHOR_RETRIES {
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
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not re-anchor the inline viewport: {}",
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
            _lock: None,
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
            _lock: None,
        };
        assert!(split_argv(&req).is_none());
    }
}
