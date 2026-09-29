//! The proposal chip: one agent-proposed command or action shown under the
//! composer (`propose` tool). Key handling and the `/restart` worker live
//! here so `events.rs` only gains a dispatch hook (CLAUDE.md rule 5).
//!
//! Principle C: nothing runs that the user has not seen as the thing being
//! run. Insert-only chips (`shell`, `slash`) are only ever put into an empty
//! composer by `Tab`; `Enter` never sends them. Only an executable chip
//! (`restart`, always the current agent) runs on `Enter`, and only when idle.
//!
//! Design: `docs/superpowers/specs/2026-09-28-murmur-proposal-chip-design.md`.

use super::*;
use mur_common::proposal::Proposal;

/// How long a real chip stays up once the turn that offered it has ended.
/// Long enough to read the command; short enough that a stale suggestion does
/// not squat on the composer for the rest of the session.
pub(super) const PROPOSAL_TTL: std::time::Duration = std::time::Duration::from_secs(120);

/// Outcome of a chip-triggered restart, as delivered to the UI loop.
pub(crate) type RestartOutcome = std::result::Result<crate::cmd::agent::QuietRestart, String>;

/// Whether `Tab` belongs to the chip even though the completion overlay is
/// open. The `suggest_replies` chooser (`spaced`) never advertises Tab — its
/// keys are digits, arrows and Enter — while the chip row says "Tab to
/// insert", so on an empty composer Tab must reach the chip instead of
/// accepting the chooser's highlighted reply. The slash menu (not `spaced`)
/// only opens on a non-empty composer and keeps Tab.
pub(super) fn owns_tab_over_chooser(app: &App, key: &crossterm::event::KeyEvent) -> bool {
    key.code == KeyCode::Tab
        && key.modifiers.is_empty()
        && app.completion.as_ref().is_some_and(|c| c.spaced)
        && app.input_text().is_empty()
        && app
            .proposal
            .as_ref()
            .is_some_and(|p| !p.is_reply() && p.insert_text().is_some())
}

/// Handle a keypress for the chip. `true` means the key was consumed.
///
/// Called after HITL and the completion overlay have had their turn —
/// priority HITL > overlay > chip. The ghost is a chip too
/// (`ProposalKind::Reply`); it only ever answers `Tab`, as it always has.
pub(super) fn handle_key(
    app: &mut App,
    key: &crossterm::event::KeyEvent,
    tx: &mpsc::Sender<StreamMsg>,
) -> bool {
    handle_key_with(app, key, tx, run_restart)
}

/// [`handle_key`] with the restart runner injected, so tests never restart
/// a real agent.
pub(super) fn handle_key_with(
    app: &mut App,
    key: &crossterm::event::KeyEvent,
    tx: &mpsc::Sender<StreamMsg>,
    runner: fn(&str) -> RestartOutcome,
) -> bool {
    let Some(chip) = app.proposal.as_ref() else {
        return false;
    };
    if is_pin_key(key) && !chip.is_reply() && app.input_text().is_empty() {
        toggle_pin(app);
        return true;
    }
    // Modified keys (Shift/Alt+Enter = newline, Ctrl+…) are never the chip's.
    if !key.modifiers.is_empty() {
        return false;
    }
    let empty = app.input_text().is_empty();
    // The ghost keeps its original rule: Tab fills an empty composer; every
    // other key keeps its ordinary meaning (Enter still submits and clears it).
    if chip.is_reply() && key.code != KeyCode::Tab {
        return false;
    }
    match key.code {
        KeyCode::Tab if empty => match chip.insert_text() {
            Some(text) => {
                app.set_input(&text);
                app.proposal = None;
                true
            }
            // Executable chips have nothing to insert; Tab keeps its
            // ordinary meaning (slash menu).
            None => false,
        },
        // An image-only send is still a send; let `submit` have it.
        KeyCode::Enter if empty && app.pending_image.is_none() => {
            if chip.is_executable() && !app.streaming && !app.restart_in_flight {
                let label = chip.label.clone();
                start_restart(app, tx, label, runner);
            }
            // Insert-only: nothing sent, nothing inserted. Executable while a
            // turn streams: inert (Q7) — the chip hint says why.
            true
        }
        KeyCode::Esc if empty && !app.streaming => {
            app.proposal = None;
            true
        }
        _ => false,
    }
}

/// `Ctrl+K` ("keep"): pins or unpins the chip. Unbound elsewhere in the
/// composer loop and in the completion overlay; on an empty composer the
/// textarea's own kill-to-end-of-line has nothing to kill.
fn is_pin_key(key: &crossterm::event::KeyEvent) -> bool {
    key.code == KeyCode::Char('k') && key.modifiers == crossterm::event::KeyModifiers::CONTROL
}

/// Pin stops the countdown for good; unpin gives the chip a fresh one
/// (re-armed by `tick`).
fn toggle_pin(app: &mut App) {
    app.proposal_pinned = !app.proposal_pinned;
    app.proposal_deadline = None;
}

/// Run the restart on a worker thread; the result comes back as
/// `StreamMsg::RestartDone`. Locks turn submission until it lands.
fn start_restart(
    app: &mut App,
    tx: &mpsc::Sender<StreamMsg>,
    label: String,
    runner: fn(&str) -> RestartOutcome,
) {
    app.proposal = None;
    app.restart_in_flight = true;
    let agent = app.agent.clone();
    app.push_system(format!("↻ restarting {agent} — {label}"));
    let tx = tx.clone();
    std::thread::spawn(move || {
        let outcome = runner(&agent);
        let _ = tx.blocking_send(StreamMsg::RestartDone(outcome));
    });
}

fn run_restart(agent: &str) -> RestartOutcome {
    crate::cmd::agent::restart_quiet(agent).map_err(|e| format!("{e:#}"))
}

/// Apply a finished restart: unlock submission and render the progress lines
/// the CLI would have printed. The conversation id is left alone — the
/// runtime's `ConversationStore` is disk-backed, so the next turn resumes it.
pub(super) fn finish_restart(app: &mut App, outcome: RestartOutcome) {
    app.restart_in_flight = false;
    match outcome {
        Ok(r) => {
            for (failed, line) in r.notes {
                if failed {
                    app.push_error(format!("✗ {line}"));
                } else {
                    app.push_system(line);
                }
            }
            if r.ok {
                app.push_system(format!("✓ {}", r.detail));
            } else if !app
                .messages
                .last()
                .is_some_and(|m| m.text.contains(&r.detail))
            {
                app.push_error(format!("✗ {}", r.detail));
            }
        }
        Err(e) => app.push_error(format!("✗ restart failed: {e}")),
    }
}

/// Offer a chip. A newer proposal replaces the old one — including a ghost,
/// whose placeholder text goes with it.
pub(super) fn offer(app: &mut App, p: Proposal) {
    app.clear_suggestion_ghost();
    app.proposal = Some(p);
    // A fresh chip gets a fresh, unpinned countdown, armed by `tick` once idle.
    app.proposal_deadline = None;
    app.proposal_pinned = false;
}

/// Whether the slot holds a real (rendered-row) chip rather than the ghost.
fn has_real_chip(app: &App) -> bool {
    app.proposal.as_ref().is_some_and(|p| !p.is_reply())
}

/// Advance the chip countdown; run at the top of every loop pass. Arms the
/// deadline once no turn is streaming (a chip offered mid-turn must not tick
/// away while the user is still reading the reply), and retires the chip when
/// it lapses. The ghost never expires — it is placeholder text, not a row.
/// Returns `true` when the chip was dismissed.
pub(super) fn tick(app: &mut App, now: std::time::Instant) -> bool {
    if !has_real_chip(app) {
        app.proposal_deadline = None;
        return false;
    }
    if app.streaming || app.restart_in_flight || app.proposal_pinned {
        app.proposal_deadline = None;
        return false;
    }
    let deadline = *app.proposal_deadline.get_or_insert(now + PROPOSAL_TTL);
    if now >= deadline {
        app.proposal = None;
        app.proposal_deadline = None;
        return true;
    }
    false
}

/// When the event loop must next wake for the chip: the next whole-second
/// step of the visible countdown, capped at the deadline itself. `None` when
/// no countdown is running.
pub(super) fn next_wake(app: &App, now: std::time::Instant) -> Option<std::time::Instant> {
    let deadline = app.proposal_deadline.filter(|_| has_real_chip(app))?;
    let left = deadline.saturating_duration_since(now);
    let frac = std::time::Duration::from_nanos((left.as_nanos() % 1_000_000_000) as u64);
    let step = if frac.is_zero() {
        std::time::Duration::from_secs(1)
    } else {
        frac
    };
    Some((now + step).min(deadline))
}

/// Whole seconds left on the countdown, rounded up (so it never shows `0s`
/// while the chip is still up).
fn secs_left(app: &App, now: std::time::Instant) -> Option<u64> {
    let left = app.proposal_deadline?.saturating_duration_since(now);
    Some(left.as_millis().div_ceil(1000) as u64)
}

/// The one line rendered above the composer: what the chip is, then which key
/// does what right now. The command itself is always shown (principle C).
pub(super) fn chip_line(app: &App) -> Option<(String, String)> {
    // The ghost renders as the composer's placeholder, not a chip row.
    let p = app.proposal.as_ref().filter(|p| !p.is_reply())?;
    let what = match p.insert_text() {
        Some(text) => format!("⤷ {text} — {}", p.label),
        None => format!("⤷ restart {} — {}", app.agent, p.label),
    };
    let empty = app.input_text().is_empty();
    let pin = if app.proposal_pinned {
        "Ctrl+K to unpin"
    } else {
        "Ctrl+K to keep"
    };
    let hint = if p.is_executable() {
        if app.streaming || app.restart_in_flight {
            "(Enter to run after the turn ends)"
        } else if empty {
            "Enter to run · Esc to dismiss"
        } else {
            "(clear the input, then Enter to run)"
        }
    } else if empty {
        "Tab to insert · Esc to dismiss"
    } else {
        "(clear to Tab-insert)"
    };
    let mut hint = hint.to_string();
    if empty {
        hint = format!("{hint} · {pin}");
    }
    if app.proposal_pinned {
        hint.push_str(" · pinned");
    } else if let Some(s) = secs_left(app, std::time::Instant::now()) {
        hint = format!("{hint} · {s}s");
    }
    Some((what, hint))
}

#[cfg(test)]
#[path = "proposal_tests.rs"]
mod tests;
