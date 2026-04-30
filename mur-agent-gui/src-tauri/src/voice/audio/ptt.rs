//! Push-to-talk state machine.
//!
//! Lifecycle:
//!   Idle — HotkeyDown → Recording — HotkeyUp → Transcribing → Idle
//!
//! Holds shorter than `MIN_HOLD_MS` are debounced (modifier
//! double-taps, accidental brushes against the rebound key).

use std::time::{Duration, Instant};

const MIN_HOLD_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PttState {
    Idle,
    Recording { started_at: Instant },
    Transcribing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PttEvent {
    HotkeyDown,
    HotkeyUp,
    TranscribeDone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PttAction {
    /// Stray event (e.g., key-repeat); take no action.
    None,
    /// Start mic capture.
    StartCapture,
    /// Stop capture and dispatch to STT. `hold_ms` is informational
    /// (telemetry / waveform UI).
    StopCaptureAndTranscribe { hold_ms: u64 },
    /// Hold was below the debounce threshold; abort capture without
    /// transcribing. The GUI still ends the visual "recording" state.
    Suppressed,
}

pub struct PttFsm {
    state: PttState,
}

impl Default for PttFsm {
    fn default() -> Self {
        Self {
            state: PttState::Idle,
        }
    }
}

impl PttFsm {
    pub fn state(&self) -> PttState {
        self.state
    }

    pub fn handle(&mut self, ev: PttEvent) -> PttAction {
        match (self.state, ev) {
            (PttState::Idle, PttEvent::HotkeyDown) => {
                self.state = PttState::Recording {
                    started_at: Instant::now(),
                };
                PttAction::StartCapture
            }
            (PttState::Recording { started_at }, PttEvent::HotkeyUp) => {
                let hold = started_at.elapsed();
                if hold < Duration::from_millis(MIN_HOLD_MS) {
                    self.state = PttState::Idle;
                    PttAction::Suppressed
                } else {
                    self.state = PttState::Transcribing;
                    PttAction::StopCaptureAndTranscribe {
                        hold_ms: hold.as_millis() as u64,
                    }
                }
            }
            (PttState::Transcribing, PttEvent::TranscribeDone) => {
                self.state = PttState::Idle;
                PttAction::None
            }
            // Stray events: key-repeat HotkeyDown while Recording, an
            // unsolicited TranscribeDone while Idle, etc. Ignore.
            _ => PttAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn happy_path_capture_then_transcribe() {
        let mut fsm = PttFsm::default();
        assert_eq!(fsm.handle(PttEvent::HotkeyDown), PttAction::StartCapture);
        sleep(Duration::from_millis(MIN_HOLD_MS + 10));
        match fsm.handle(PttEvent::HotkeyUp) {
            PttAction::StopCaptureAndTranscribe { hold_ms } => {
                assert!(hold_ms >= MIN_HOLD_MS);
            }
            other => panic!("unexpected action: {other:?}"),
        }
        assert_eq!(fsm.handle(PttEvent::TranscribeDone), PttAction::None);
        assert_eq!(fsm.state(), PttState::Idle);
    }

    #[test]
    fn short_press_is_suppressed() {
        let mut fsm = PttFsm::default();
        fsm.handle(PttEvent::HotkeyDown);
        // Release immediately — well under MIN_HOLD_MS.
        assert_eq!(fsm.handle(PttEvent::HotkeyUp), PttAction::Suppressed);
        assert_eq!(fsm.state(), PttState::Idle);
    }

    #[test]
    fn key_repeat_during_recording_is_ignored() {
        let mut fsm = PttFsm::default();
        fsm.handle(PttEvent::HotkeyDown);
        // OS key-repeat → another HotkeyDown while already Recording.
        assert_eq!(fsm.handle(PttEvent::HotkeyDown), PttAction::None);
        // Still in Recording.
        assert!(matches!(fsm.state(), PttState::Recording { .. }));
    }

    #[test]
    fn stray_transcribe_done_in_idle_is_ignored() {
        let mut fsm = PttFsm::default();
        assert_eq!(fsm.handle(PttEvent::TranscribeDone), PttAction::None);
        assert_eq!(fsm.state(), PttState::Idle);
    }

    #[test]
    fn hotkey_up_in_idle_is_ignored() {
        let mut fsm = PttFsm::default();
        assert_eq!(fsm.handle(PttEvent::HotkeyUp), PttAction::None);
        assert_eq!(fsm.state(), PttState::Idle);
    }

    #[test]
    fn hotkey_up_during_transcribing_is_ignored() {
        let mut fsm = PttFsm::default();
        fsm.handle(PttEvent::HotkeyDown);
        sleep(Duration::from_millis(MIN_HOLD_MS + 5));
        let _ = fsm.handle(PttEvent::HotkeyUp); // → Transcribing
        // User starts pressing again while still transcribing — ignored.
        assert_eq!(fsm.handle(PttEvent::HotkeyDown), PttAction::None);
        assert_eq!(fsm.state(), PttState::Transcribing);
    }
}
