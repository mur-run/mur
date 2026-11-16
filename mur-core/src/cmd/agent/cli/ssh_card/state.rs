//! The card's state machine, free of I/O: three steps over one `SetupPlan`
//! (SSH setup spec, "The card"). Every decision the user can make lands in
//! `CardState`; `approval` turns it into the one `Approval` that
//! `apply_and_restart` accepts, or says why Approve is unavailable.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mur_common::agent::is_fingerprint;
use mur_ssh_request::PendingRequest;

use crate::cmd::agent::ssh::setup::apply::approved_known_hosts;
use crate::cmd::agent::ssh::setup::choices::{Choices, NotReady, build_approval};
use crate::cmd::agent::ssh::setup::plan::Dedicated;
use crate::cmd::agent::ssh::setup::probe::{AgentKey, private_key_of};
use crate::cmd::agent::ssh::setup::{Approval, SetupPlan, SpawnChange, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Key,
    Host,
    Summary,
    /// Written, not restarted: the public-key install failed (Journey 4.3).
    /// `r` retries it, `y` copies the line, Enter restarts anyway.
    Held,
    /// Written with the dedicated key and no install: the line to add by
    /// hand, and `t` for the advisory test.
    Done,
}

/// The row under the key list that is not a key (SSH setup spec, card
/// step 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtraRow {
    /// "generate a dedicated key for this agent".
    Generate,
    /// Shown, not selectable, with the reason.
    Off(String),
}

/// How the card was opened, which decides what is handed back to the agent
/// after a successful restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// `/ssh <host>` typed by the user: nothing to hand back.
    Direct,
    /// Picked from the pending list: no user text, so the agent gets its own
    /// reason back with "run one test connection".
    Pending(PendingRequest),
    /// The streamed `ssh_request` call: the user text that triggered it is
    /// replayed (the `TurnLost` resend path).
    Tool {
        request: PendingRequest,
        resend: Option<String>,
    },
}

impl Origin {
    pub fn request(&self) -> Option<&PendingRequest> {
        match self {
            Self::Direct => None,
            Self::Pending(r) | Self::Tool { request: r, .. } => Some(r),
        }
    }

    /// The message that resumes the agent's work, if any.
    pub fn hand_back(&self) -> Option<String> {
        match self {
            Self::Direct => None,
            Self::Tool {
                resend: Some(t), ..
            } => Some(t.clone()),
            Self::Pending(r) | Self::Tool { request: r, .. } => {
                Some(crate::cmd::agent::ssh::setup::pending::hand_back_text(r))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct CardState {
    pub plan: SetupPlan,
    pub origin: Origin,
    pub step: Step,
    /// Index into `keys()`.
    pub key_idx: usize,
    /// Index into `plan.scanned`.
    pub host_idx: usize,
    /// Scanned fingerprints ticked "I compared this".
    pub ticked: Vec<String>,
    /// A fingerprint the user pasted.
    pub pasted: Option<String>,
    pub spawn_consented: bool,
    /// The login user; only used for the test connection and key install.
    pub user: String,
    pub editing_user: bool,
    /// `ssh-add` was handed the terminal for the chosen key; the next
    /// `refresh_keys` decides whether the card moves on.
    pub loading: bool,
    /// A one-line hint from the last key press.
    pub note: Option<String>,
    /// Install the dedicated public key on the host after the write (`i`).
    pub install: bool,
    /// What Approve wrote, kept for the held and done steps.
    pub applied: Option<Approval>,
}

pub enum KeyAction {
    Nothing,
    /// Close before anything was written.
    Close,
    /// Close from the held or done step: the grant is already written.
    /// `held` is the held step, where the agent was not restarted.
    CloseWritten {
        held: bool,
    },
    /// Run `ssh-add <path>` with the terminal handed over; `path` is the
    /// private key, never the `.pub` the card lists.
    LoadKey(PathBuf),
    Approve(Approval),
    /// Generate (or load) this agent's dedicated key off the UI thread.
    Generate,
    /// Held step: install again; on success, restart.
    RetryInstall(Approval),
    /// Held step: restart without the install.
    Finish(Approval),
    /// Copy the `authorized_keys` line to the terminal's clipboard.
    Copy(String),
    /// Done step: the advisory test, outside the agent's sandbox.
    Advisory {
        host: mur_ssh_request::RequestHost,
        user: String,
        known_hosts: String,
    },
}

impl CardState {
    pub fn new(plan: SetupPlan, origin: Origin) -> Self {
        let user = plan.user.clone().unwrap_or_default();
        Self {
            plan,
            origin,
            step: Step::Key,
            key_idx: 0,
            host_idx: 0,
            ticked: Vec::new(),
            pasted: None,
            spawn_consented: false,
            user,
            editing_user: false,
            loading: false,
            note: None,
            install: false,
            applied: None,
        }
    }

    /// The row after the keys, if the plan has one.
    pub fn extra_row(&self) -> Option<ExtraRow> {
        match &self.plan.dedicated {
            Dedicated::Offer => Some(ExtraRow::Generate),
            Dedicated::Off(off) => Some(ExtraRow::Off(off.reason())),
            // Listed first among the keys by `SetupPlan::set_keys`.
            Dedicated::Stored { .. } => None,
        }
    }

    /// Keys plus the extra row: what ↑↓ moves over on the key step.
    fn key_rows(&self) -> usize {
        self.keys().len() + usize::from(self.extra_row().is_some())
    }

    /// The dedicated key's `authorized_keys` line, when it is the chosen key.
    pub fn dedicated_line(&self) -> Option<&str> {
        let (k, _) = self.chosen_key()?;
        match &self.plan.dedicated {
            Dedicated::Stored {
                fingerprint,
                public_line,
            } if *fingerprint == k.fingerprint => Some(public_line),
            _ => None,
        }
    }

    /// A key was generated: take the new plan state and key lists,
    /// with the selection on `fingerprint`. Grants nothing (K2).
    pub fn generated(
        &mut self,
        dedicated: Dedicated,
        loaded: Vec<AgentKey>,
        unloaded: Vec<AgentKey>,
        fingerprint: &str,
    ) {
        self.plan.dedicated = dedicated;
        self.plan.set_keys(loaded, unloaded);
        self.key_idx = self
            .keys()
            .iter()
            .position(|(k, _)| k.fingerprint == fingerprint)
            .unwrap_or(0);
        self.note = Some("dedicated key ready; Enter to use it".into());
    }

    /// Approve came back: stay open on `Held` or `Done`, or say the card is
    /// finished. `held` is `Restart::HeldForInstall`.
    pub fn after_apply(&mut self, a: Approval, held: bool) -> bool {
        let no_install = a.install.is_none();
        self.applied = Some(a);
        self.note = None;
        if held {
            self.step = Step::Held;
        } else if no_install && self.dedicated_line().is_some() {
            self.step = Step::Done;
        } else {
            return false;
        }
        true
    }

    /// Loaded keys first, then unloaded `.pub` files; `true` when loaded.
    pub fn keys(&self) -> Vec<(&AgentKey, bool)> {
        let loaded = self.plan.keys_loaded.iter().map(|k| (k, true));
        let unloaded = self.plan.keys_unloaded.iter().map(|k| (k, false));
        loaded.chain(unloaded).collect()
    }

    pub fn chosen_key(&self) -> Option<(&AgentKey, bool)> {
        self.keys().get(self.key_idx).copied()
    }

    /// Take a fresh read of the key lists (after `ssh-add`), keeping the
    /// selection on the same key: a key that just loaded moves from the
    /// unloaded tail to the loaded head, so its index changes.
    pub fn refresh_keys(&mut self, loaded: Vec<AgentKey>, unloaded: Vec<AgentKey>) {
        let chosen = self.chosen_key().map(|(k, _)| k.fingerprint.clone());
        self.plan.set_keys(loaded, unloaded);
        let keys = self.keys();
        self.key_idx = chosen
            .and_then(|fp| keys.iter().position(|(k, _)| k.fingerprint == fp))
            .unwrap_or(0)
            .min(keys.len().saturating_sub(1));
        if std::mem::take(&mut self.loading) && self.step == Step::Key {
            match self.chosen_key() {
                Some((_, true)) => self.step = Step::Host,
                _ => self.note = Some("ssh-add did not load the key; nothing was changed".into()),
            }
        }
    }

    /// Ticking is offered on `unknown` only (spec verdict table).
    pub fn ticks_offered(&self) -> bool {
        matches!(self.plan.verdict, Verdict::Unknown { .. })
    }

    /// A bracketed paste while the card is on the host step.
    pub fn paste(&mut self, text: &str) {
        let fp = text.trim();
        if is_fingerprint(fp) {
            self.pasted = Some(fp.to_string());
            self.note = None;
        } else {
            self.note = Some("that is not a SHA256:… fingerprint; nothing was taken".into());
        }
    }

    /// The approval the user built, or why Approve is unavailable. The rule
    /// is `setup::choices`; only the words are murmur's.
    pub fn approval(&self) -> Result<Approval, String> {
        let choices = Choices {
            key_fp: self.chosen_key().map(|(k, _)| k.fingerprint.clone()),
            pasted: self.pasted.clone(),
            ticked: self.ticked.clone(),
            spawn_consented: self.spawn_consented,
            user: self.user.clone(),
            // A toggle left on while another key is chosen is not an install.
            install: self.install && self.dedicated_line().is_some(),
        };
        build_approval(&self.plan, &choices).map_err(|e| match e {
            NotReady::NoKey => "no key to grant: load one with ssh-add, then reopen /ssh".into(),
            NotReady::KeyNotLoaded => {
                "the chosen key is not loaded in ssh-agent; go back and load it".into()
            }
            NotReady::SpawnConsent => "tick the process-permission consent (c) first".into(),
            NotReady::Host(s) | NotReady::User(s) | NotReady::Install(s) => s,
        })
    }

    pub fn on_key(&mut self, key: KeyEvent) -> KeyAction {
        if self.editing_user {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => self.editing_user = false,
                KeyCode::Backspace => {
                    self.user.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.user.push(c)
                }
                _ => {}
            }
            return KeyAction::Nothing;
        }
        self.note = None;
        if matches!(self.step, Step::Held | Step::Done) {
            return self.on_result_key(key.code);
        }
        match key.code {
            KeyCode::Esc => return KeyAction::Close,
            KeyCode::Char('u') => self.editing_user = true,
            KeyCode::Left | KeyCode::Backspace => self.back(),
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Down => self.move_sel(1),
            KeyCode::Char(' ') if self.step == Step::Host => self.toggle_tick(),
            KeyCode::Char('c') if self.step == Step::Summary => {
                if self.plan.spawn == Some(SpawnChange::NoneToStrict) {
                    self.spawn_consented = !self.spawn_consented;
                }
            }
            KeyCode::Char('i') if self.step == Step::Summary => self.toggle_install(),
            KeyCode::Enter | KeyCode::Right => return self.advance(key.code == KeyCode::Enter),
            _ => {}
        }
        KeyAction::Nothing
    }

    fn toggle_install(&mut self) {
        if self.dedicated_line().is_none() {
            self.note = Some("only this agent's dedicated key can be installed".into());
        } else if self.user.is_empty() {
            self.note = Some("set a user first (u)".into());
        } else {
            self.install = !self.install;
        }
    }

    /// Keys on the held and done steps. Esc on either closes: the grant is
    /// already written, so there is nothing to cancel.
    fn on_result_key(&mut self, code: KeyCode) -> KeyAction {
        let held = self.step == Step::Held;
        let Some(a) = self.applied.clone() else {
            return KeyAction::CloseWritten { held };
        };
        let line = self.dedicated_line().map(str::to_string);
        match (self.step, code) {
            (_, KeyCode::Esc) => KeyAction::CloseWritten { held },
            (_, KeyCode::Char('y')) => match line {
                Some(l) => KeyAction::Copy(l),
                None => KeyAction::Nothing,
            },
            (Step::Held, KeyCode::Char('r')) => KeyAction::RetryInstall(a),
            (Step::Held, KeyCode::Enter) => KeyAction::Finish(a),
            (Step::Done, KeyCode::Char('t')) => KeyAction::Advisory {
                host: self.plan.host.clone(),
                user: self.user.clone(),
                known_hosts: approved_known_hosts(&a),
            },
            (Step::Done, KeyCode::Enter) => KeyAction::CloseWritten { held: false },
            _ => KeyAction::Nothing,
        }
    }

    fn back(&mut self) {
        self.step = match self.step {
            Step::Key | Step::Host => Step::Key,
            Step::Summary => Step::Host,
            s @ (Step::Held | Step::Done) => s,
        };
    }

    fn move_sel(&mut self, d: isize) {
        let (idx, len) = match self.step {
            Step::Key => {
                let n = self.key_rows();
                (&mut self.key_idx, n)
            }
            Step::Host => (&mut self.host_idx, self.plan.scanned.len()),
            Step::Summary | Step::Held | Step::Done => return,
        };
        if len > 0 {
            *idx = (*idx as isize + d).clamp(0, len as isize - 1) as usize;
        }
    }

    fn toggle_tick(&mut self) {
        if !self.ticks_offered() {
            self.note = Some(match self.plan.verdict {
                Verdict::Differs => "ticking is not offered when the stored key differs; paste \
                                     the operator's fingerprint"
                    .into(),
                _ => "already trusted; nothing to tick".into(),
            });
            return;
        }
        let Some(k) = self.plan.scanned.get(self.host_idx) else {
            return;
        };
        let fp = k.fingerprint.clone();
        match self.ticked.iter().position(|t| *t == fp) {
            Some(i) => {
                self.ticked.remove(i);
            }
            None => self.ticked.push(fp),
        }
    }

    fn advance(&mut self, enter: bool) -> KeyAction {
        match self.step {
            Step::Key => {
                if self.key_idx == self.keys().len() {
                    return match self.extra_row() {
                        Some(ExtraRow::Generate) => KeyAction::Generate,
                        Some(ExtraRow::Off(why)) => {
                            self.note = Some(format!("no dedicated key: {why}"));
                            KeyAction::Nothing
                        }
                        None => KeyAction::Nothing,
                    };
                }
                let Some((key, loaded)) = self.chosen_key() else {
                    self.note = Some("no key to choose".into());
                    return KeyAction::Nothing;
                };
                if loaded {
                    self.step = Step::Host;
                    return KeyAction::Nothing;
                }
                // Stay here until `refresh_keys` sees the key in ssh-agent.
                let Some(p) = key.path.as_deref().map(private_key_of) else {
                    self.note = Some("no private key file for this key".into());
                    return KeyAction::Nothing;
                };
                self.loading = true;
                return KeyAction::LoadKey(p);
            }
            Step::Host => self.step = Step::Summary,
            Step::Summary if enter => match self.approval() {
                Ok(a) => return KeyAction::Approve(a),
                Err(e) => self.note = Some(e),
            },
            Step::Summary | Step::Held | Step::Done => {}
        }
        KeyAction::Nothing
    }
}
