//! The browser card's state machine: pure, so every key is testable without
//! an `App`. `mod.rs` turns a [`KeyAction`] into background work.

use crossterm::event::{KeyCode, KeyEvent};

use crate::cmd::browser::request_card::front::{
    Card, LoginAgain, Offer, Pick, ProfileRow, Reach, Shown, Status,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Choosing,
    /// The login browser is open; Enter = "logged in", Esc = cancel (D6).
    LoggingIn,
    /// A settle is running off the UI thread.
    Working,
    /// A login was saved; `status` is the D4 label.
    Saved {
        profile: String,
        status: Status,
    },
}

#[derive(Debug, Clone)]
pub struct State {
    pub shown: Shown,
    /// Index into the login card's rows; `rows().len()` is the "log in"
    /// line, so the login can be the default and take Enter and `r` (#2461).
    pub selected: usize,
    pub phase: Phase,
    pub note: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyAction {
    Nothing,
    /// Close; nothing was written.
    Close,
    Settle(Pick),
    /// Enter while `LoggingIn`: resolve the "logged in" oneshot.
    LoggedIn,
    /// Esc while `LoggingIn`: drop the oneshot, nothing saved.
    CancelLogin,
}

impl State {
    pub fn new(shown: Shown) -> Self {
        let mut s = Self {
            shown,
            selected: 0,
            phase: Phase::Choosing,
            note: None,
        };
        s.selected = s.default_selection();
        s
    }

    /// A usable row whose session is for this site; else the login line;
    /// else any usable row. Enter never quietly picks a profile that logged
    /// in somewhere else (#2461).
    fn default_selection(&self) -> usize {
        let rows = self.rows();
        let with_session =
            (0..rows.len()).find(|&i| self.enabled(i) && rows[i].session_elsewhere.is_none());
        with_session
            .or_else(|| self.enabled(self.login_at()).then_some(self.login_at()))
            .or_else(|| self.first_enabled())
            .unwrap_or(0)
    }

    /// The cursor position of the "log in" line.
    pub fn login_at(&self) -> usize {
        self.rows().len()
    }

    fn login(&self) -> Option<(&LoginAgain, &Offer)> {
        match &self.shown.card {
            Card::Login {
                login_again,
                login_offer,
                ..
            } => Some((login_again, login_offer)),
            _ => None,
        }
    }

    pub fn rows(&self) -> &[ProfileRow] {
        match &self.shown.card {
            Card::Login { rows, .. } => rows,
            _ => &[],
        }
    }

    /// The offer at cursor position `i`: a row's, or the login line's.
    fn offer_at(&self, i: usize) -> Option<&Offer> {
        match self.rows().get(i) {
            Some(r) => Some(&r.offer),
            None if i == self.login_at() => self.login().map(|(_, o)| o),
            None => None,
        }
    }

    fn enabled(&self, i: usize) -> bool {
        self.offer_at(i)
            .is_some_and(|o| !matches!(o, Offer::Disabled { .. }))
    }

    fn first_enabled(&self) -> Option<usize> {
        (0..self.rows().len()).find(|&i| self.enabled(i))
    }

    /// Move the selection over the rows and the login line, skipping
    /// disabled ones.
    fn step(&mut self, down: bool) {
        let n = self.rows().len() + 1;
        let mut i = self.selected;
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if self.enabled(i) {
                self.selected = i;
                return;
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> KeyAction {
        match self.phase {
            Phase::Working => KeyAction::Nothing,
            Phase::LoggingIn => match key.code {
                KeyCode::Enter => KeyAction::LoggedIn,
                KeyCode::Esc => KeyAction::CancelLogin,
                _ => KeyAction::Nothing,
            },
            Phase::Saved { .. } => match key.code {
                KeyCode::Esc | KeyCode::Enter => KeyAction::Close,
                _ => KeyAction::Nothing,
            },
            Phase::Choosing => self.choosing(key),
        }
    }

    fn choosing(&mut self, key: KeyEvent) -> KeyAction {
        match (key.code, &self.shown.card) {
            (KeyCode::Esc, _) => KeyAction::Close,
            (KeyCode::Char('d'), Card::Login { .. } | Card::Reach { .. }) => {
                KeyAction::Settle(Pick::Dismiss)
            }
            (KeyCode::Up, Card::Login { .. }) => {
                self.step(false);
                KeyAction::Nothing
            }
            (KeyCode::Down, Card::Login { .. }) => {
                self.step(true);
                KeyAction::Nothing
            }
            (KeyCode::Enter, Card::Login { .. }) => {
                if !self.enabled(self.selected) {
                    self.note =
                        Some("that row is not available — the reason is shown above".into());
                    return KeyAction::Nothing;
                }
                if self.selected == self.login_at() {
                    return self.log_in(false);
                }
                let row = &self.rows()[self.selected];
                if let Offer::ReplaceOnly { drops, .. } = &row.offer {
                    self.note = Some(format!(
                        "this replaces {} — press r to replace, or pick another row",
                        drops.join(", ")
                    ));
                    return KeyAction::Nothing;
                }
                KeyAction::Settle(Pick::Row {
                    name: row.name.clone(),
                    replace: false,
                })
            }
            // The explicit replace choice a `ReplaceOnly` row needs (D4).
            (KeyCode::Char('r'), Card::Login { .. }) => {
                if !matches!(
                    self.offer_at(self.selected),
                    Some(Offer::ReplaceOnly { .. })
                ) {
                    return KeyAction::Nothing;
                }
                if self.selected == self.login_at() {
                    return self.log_in(true);
                }
                KeyAction::Settle(Pick::Row {
                    name: self.rows()[self.selected].name.clone(),
                    replace: true,
                })
            }
            (KeyCode::Char('l'), Card::Login { .. }) => {
                self.selected = self.login_at();
                self.log_in(false)
            }
            (KeyCode::Char('a') | KeyCode::Enter, Card::Reach { reach, .. }) => {
                if let Reach::Disabled { why } = reach {
                    self.note = Some(format!("not available: {why}"));
                    return KeyAction::Nothing;
                }
                KeyAction::Settle(Pick::Allow)
            }
            _ => KeyAction::Nothing,
        }
    }

    /// The login line was chosen. A login that only binds as a replacement
    /// waits for `r` (#2461), the same explicit choice a row needs; one
    /// that cannot bind says why. Either way no browser opens yet.
    fn log_in(&mut self, replace: bool) -> KeyAction {
        let Some((again, offer)) = self.login() else {
            return KeyAction::Nothing;
        };
        let reauth = again.exists;
        match offer {
            Offer::Disabled { why } => {
                self.note = Some(format!("logging in is not available: {why}"));
                KeyAction::Nothing
            }
            Offer::ReplaceOnly { drops, .. } if !replace => {
                self.note = Some(format!(
                    "logging in as “{}” replaces {} — press r to replace and log in",
                    again.name,
                    drops.join(", ")
                ));
                KeyAction::Nothing
            }
            _ => KeyAction::Settle(Pick::Login {
                name: None,
                reauth,
                replace,
            }),
        }
    }
}
