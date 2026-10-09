//! Esc and overlay key decisions, moved out of `app/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: every item below is verbatim.

use super::*;

pub const ESC_DOUBLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
pub enum EscAction {
    Arm,
    ClearInput,
    CancelAndRestore,
    Nothing,
    /// P3b-§6.1: first Esc during a review turn — pause after this turn.
    RequestPause,
    /// P3b-§6.1: second Esc inside the window — abort the in-flight turn.
    AbortTurn,
}

/// Where a MURMUR-hosted review session stands, as far as Esc cares
/// (P3b-§6.1). `Detached` is every non-review conversation and must behave
/// exactly as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewEsc {
    #[default]
    Detached,
    AwaitingConfirm,
    #[allow(dead_code)] // wired in PR 4 (Task 8)
    TurnInFlight,
}

/// Pure function — no wall-clock calls, fully testable.
pub fn esc_action(
    last_esc_at: Option<std::time::Instant>,
    streaming: bool,
    input_empty: bool,
    review: ReviewEsc,
) -> EscAction {
    match review {
        ReviewEsc::Detached => detached_esc_action(last_esc_at, streaming, input_empty),
        ReviewEsc::TurnInFlight => {
            if within_window(last_esc_at) {
                EscAction::AbortTurn
            } else {
                EscAction::RequestPause
            }
        }
        // Nothing is in flight at the send prompt: Esc only clears the note
        // being typed, so `streaming` is deliberately ignored.
        ReviewEsc::AwaitingConfirm => match (within_window(last_esc_at), input_empty) {
            (true, false) => EscAction::ClearInput,
            (false, false) => EscAction::Arm,
            (_, true) => EscAction::Nothing,
        },
    }
}

fn within_window(last_esc_at: Option<std::time::Instant>) -> bool {
    last_esc_at.is_some_and(|t| t.elapsed() < ESC_DOUBLE_WINDOW)
}

/// Today's behaviour, verbatim, for a conversation with no review attached.
fn detached_esc_action(
    last_esc_at: Option<std::time::Instant>,
    streaming: bool,
    input_empty: bool,
) -> EscAction {
    if let Some(t) = last_esc_at
        && t.elapsed() < ESC_DOUBLE_WINDOW
    {
        return if streaming {
            EscAction::CancelAndRestore
        } else if !input_empty {
            EscAction::ClearInput
        } else {
            EscAction::Nothing
        };
    }
    // First press (or window expired)
    if streaming || !input_empty {
        EscAction::Arm
    } else {
        EscAction::Nothing
    }
}

/// Result of a keypress while the transcript overlay (Ctrl+O) is open.
#[derive(Debug, PartialEq, Eq)]
pub enum OverlayKeyAction {
    /// Close the overlay and resume normal input.
    Close,
    /// Close the overlay, then request the app quit (Ctrl+D while reading).
    CloseAndQuit,
    /// Swallow the key — never inserted into the composer.
    Ignore,
}

/// Pure function — no IO, fully testable. The transcript overlay only
/// recognises Esc/Enter (return to chat) and Ctrl+D (quit); every other key
/// is swallowed so it can never leak into the input box once the overlay
/// closes.
pub fn overlay_key_action(code: KeyCode, modifiers: KeyModifiers) -> OverlayKeyAction {
    if code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL) {
        return OverlayKeyAction::CloseAndQuit;
    }
    match code {
        KeyCode::Esc | KeyCode::Enter => OverlayKeyAction::Close,
        _ => OverlayKeyAction::Ignore,
    }
}
