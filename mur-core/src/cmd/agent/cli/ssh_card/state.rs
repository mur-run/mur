//! The card's state machine, free of I/O: three steps over one `SetupPlan`
//! (SSH setup spec, "The card"). Every decision the user can make lands in
//! `CardState`; `approval` turns it into the one `Approval` that
//! `apply_and_restart` accepts, or says why Approve is unavailable.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mur_common::agent::is_fingerprint;
use mur_ssh_request::PendingRequest;
use mur_ssh_request::validate_user;

use crate::cmd::agent::ssh::edit::HostGrant;
use crate::cmd::agent::ssh::setup::probe::{AgentKey, private_key_of};
use crate::cmd::agent::ssh::setup::verdict::{self, HostChoice};
use crate::cmd::agent::ssh::setup::{Approval, SetupPlan, SpawnChange, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Key,
    Host,
    Summary,
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
            Self::Pending(r) | Self::Tool { request: r, .. } => Some(format!(
                "SSH access to {} is set up (you asked: \"{}\"). Run one test connection and \
                 report the result.",
                r.target.canonical(),
                r.reason
            )),
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
    /// A one-line hint from the last key press.
    pub note: Option<String>,
}

pub enum KeyAction {
    Nothing,
    Close,
    /// Run `ssh-add <path>` with the terminal handed over; `path` is the
    /// private key, never the `.pub` the card lists.
    LoadKey(PathBuf),
    Approve(Approval),
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
            note: None,
        }
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
        self.plan.keys_loaded = loaded;
        self.plan.keys_unloaded = unloaded;
        let keys = self.keys();
        self.key_idx = chosen
            .and_then(|fp| keys.iter().position(|(k, _)| k.fingerprint == fp))
            .unwrap_or(0)
            .min(keys.len().saturating_sub(1));
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

    /// The approval the user built, or why Approve is unavailable.
    pub fn approval(&self) -> Result<Approval, String> {
        let Some((key, _)) = self.chosen_key() else {
            return Err("no key to grant: load one with ssh-add, then reopen /ssh".into());
        };
        let host = self.plan.host.host();
        let choice = HostChoice {
            pasted: self.pasted.clone(),
            ticked: self.ticked.clone(),
        };
        let keep = verdict::select(
            host,
            &self.plan.scanned,
            &self.plan.verdict,
            &self.plan.matched,
            &choice,
        )
        .map_err(|e| e.to_string())?;
        if self.plan.spawn == Some(SpawnChange::NoneToStrict) && !self.spawn_consented {
            return Err("tick the process-permission consent (c) first".into());
        }
        if !self.user.is_empty() {
            validate_user(&self.user).map_err(|e| e.to_string())?;
        }
        let label = Some(self.plan.host.canonical());
        Ok(Approval {
            key_fp: key.fingerprint.clone(),
            key_label: (!key.comment.is_empty()).then(|| key.comment.clone()),
            hosts: keep
                .into_iter()
                .map(|k| HostGrant {
                    host_key_fp: k.fingerprint,
                    label: label.clone(),
                    known_host: Some(k.entry),
                })
                .collect(),
            spawn: self.plan.spawn,
            spawn_consented: self.spawn_consented,
            restart: true,
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
            KeyCode::Enter | KeyCode::Right => return self.advance(key.code == KeyCode::Enter),
            _ => {}
        }
        KeyAction::Nothing
    }

    fn back(&mut self) {
        self.step = match self.step {
            Step::Key | Step::Host => Step::Key,
            Step::Summary => Step::Host,
        };
    }

    fn move_sel(&mut self, d: isize) {
        let (idx, len) = match self.step {
            Step::Key => (
                &mut self.key_idx,
                self.plan.keys_loaded.len() + self.plan.keys_unloaded.len(),
            ),
            Step::Host => (&mut self.host_idx, self.plan.scanned.len()),
            Step::Summary => return,
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
                let Some((key, loaded)) = self.chosen_key() else {
                    self.note = Some("no key to choose".into());
                    return KeyAction::Nothing;
                };
                let load = (!loaded)
                    .then(|| key.path.as_deref().map(private_key_of))
                    .flatten();
                self.step = Step::Host;
                if let Some(p) = load {
                    return KeyAction::LoadKey(p);
                }
            }
            Step::Host => self.step = Step::Summary,
            Step::Summary if enter => match self.approval() {
                Ok(a) => return KeyAction::Approve(a),
                Err(e) => self.note = Some(e),
            },
            Step::Summary => {}
        }
        KeyAction::Nothing
    }
}
