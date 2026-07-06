//! P5 live-follow: permission-free polling of the terminal window's bounds.
//! Decision (2026-07-06): polling over AXObserver — zero permissions, ~600ms
//! worst-case lag is imperceptible for a companion panel.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, State};

use crate::geometry::Rect;
use crate::panel::pos::{reposition, terminal_window_bounds};

pub const FOLLOW_INTERVAL_MS: u64 = 600;

/// Generation counter: (re)starting bumps it; a running loop exits once its
/// own generation is no longer current.
#[derive(Default)]
pub struct FollowState(pub Mutex<u64>);

pub enum Tick {
    Reposition(Rect),
    Pause,
    Idle,
}

/// Pure per-tick decision. `expected_panel` is where we last placed the panel
/// (`None` on the first tick); `actual_panel` is where the window really is.
pub fn follow_tick(
    last_term: Option<Rect>,
    term: Option<Rect>,
    actual_panel: (i32, i32),
    expected_panel: Option<(i32, i32)>,
) -> Tick {
    if let Some(exp) = expected_panel
        && exp != actual_panel
    {
        return Tick::Pause; // user dragged the panel — stop fighting it
    }
    match (last_term, term) {
        (_, None) => Tick::Idle,
        (Some(a), Some(b)) if a == b => Tick::Idle,
        (_, Some(b)) => Tick::Reposition(b),
    }
}

#[tauri::command]
pub fn panel_follow(app: AppHandle, state: State<FollowState>, term_program: Option<String>) {
    let my_gen = {
        let mut g = state.0.lock().unwrap_or_else(|e| e.into_inner());
        *g += 1;
        *g
    };
    let Some(tp) = term_program else {
        return; // bump already invalidated any running loop
    };
    std::thread::spawn(move || {
        let mut last_term: Option<Rect> = None;
        let mut expected: Option<(i32, i32)> = None;
        loop {
            std::thread::sleep(Duration::from_millis(FOLLOW_INTERVAL_MS));
            let st: State<FollowState> = app.state();
            if *st.0.lock().unwrap_or_else(|e| e.into_inner()) != my_gen {
                return;
            }
            let Some(win) = app.get_webview_window(super::PANEL_LABEL) else {
                return;
            };
            let actual = match win.outer_position() {
                Ok(p) => (p.x, p.y),
                Err(_) => return,
            };
            let term = terminal_window_bounds(&win, &tp);
            match follow_tick(last_term, term, actual, expected) {
                Tick::Pause => return, // re-pin restarts via a fresh panel_follow call
                Tick::Idle => {}
                Tick::Reposition(t) => {
                    reposition(&win, Some(t));
                    expected = win.outer_position().ok().map(|p| (p.x, p.y));
                }
            }
            last_term = term;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32) -> Rect {
        Rect {
            x,
            y,
            w: 800,
            h: 600,
        }
    }

    #[test]
    fn reposition_when_terminal_moved() {
        match follow_tick(Some(r(0, 0)), Some(r(50, 0)), (900, 0), Some((900, 0))) {
            Tick::Reposition(t) => assert_eq!(t, r(50, 0)),
            _ => panic!("expected Reposition"),
        }
    }

    #[test]
    fn idle_when_nothing_changed() {
        assert!(matches!(
            follow_tick(Some(r(0, 0)), Some(r(0, 0)), (900, 0), Some((900, 0))),
            Tick::Idle
        ));
    }

    #[test]
    fn pause_when_user_dragged_panel() {
        // actual (300,300) != expected (900,0): the user moved the panel.
        assert!(matches!(
            follow_tick(Some(r(0, 0)), Some(r(50, 0)), (300, 300), Some((900, 0))),
            Tick::Pause
        ));
    }

    #[test]
    fn terminal_vanished_is_idle_not_fallback() {
        // Terminal window gone mid-session: do nothing, don't snap to a
        // fallback position.
        assert!(matches!(
            follow_tick(Some(r(0, 0)), None, (900, 0), Some((900, 0))),
            Tick::Idle
        ));
    }

    #[test]
    fn first_tick_with_no_expected_pos_repositions() {
        // No prior term bounds and no expected panel position yet (the very
        // first tick after `panel_follow` starts): still reposition.
        match follow_tick(None, Some(r(0, 0)), (0, 0), None) {
            Tick::Reposition(t) => assert_eq!(t, r(0, 0)),
            _ => panic!("expected Reposition"),
        }
    }
}
