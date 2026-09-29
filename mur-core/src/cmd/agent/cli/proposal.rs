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

/// Outcome of a chip-triggered restart, as delivered to the UI loop.
pub(crate) type RestartOutcome = std::result::Result<crate::cmd::agent::QuietRestart, String>;

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
    Some((what, hint.to_string()))
}

#[cfg(test)]
#[path = "proposal_tests.rs"]
mod tests;
