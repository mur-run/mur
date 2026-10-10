//! `MurmurTransport` (P3b-§4.1, §5, §7.1, §10): [`ReviewTransport`] for a
//! review worker thread whose human is the MURMUR UI. Every question becomes
//! a [`DriverReq`] and the worker blocks on the reply.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::super::constants::{REVIEW_PAUSE_REASON_DETACHED, REVIEW_PAUSE_REASON_USER};
use super::super::driver::{RequestedPause, ReviewTransport, SendAnswer, answer_hitl, task_reply};
use super::super::ledger::{EscalationRecord, Ledger};
use super::super::ruling::ruling_prompt_text;
use super::super::schema::PauseKind;
use super::super::turn_cell::TurnCell;
use super::bridge::{DriverReq, ReviewFlags};
use crate::cmd::agent::cli::stream::HitlRequest;

/// Audit attribution for an answer given in the MURMUR UI.
const HITL_SURFACE: &str = "cli";

/// `dial_message_streaming`, injectable: `(home, member, params, on_delta,
/// on_hitl) -> task`. `on_delta` takes `(text, thinking, task_id)`.
pub type DialFn = Arc<
    dyn Fn(
            &Path,
            &str,
            serde_json::Value,
            &mut dyn FnMut(&str, bool, &str),
            &mut dyn FnMut(serde_json::Value),
        ) -> Result<serde_json::Value>
        + Send
        + Sync,
>;

/// Delivers one approval answer: `(member, hitl_id, allow)`.
pub type RespondFn = Arc<dyn Fn(&str, &str, bool) + Send + Sync>;

/// The real network edges: A2A streaming dial, and the HITL answer call.
pub fn real_io(mur_home: &Path) -> (DialFn, RespondFn) {
    let dial: DialFn = Arc::new(|home, member, params, on_delta, on_hitl| {
        crate::a2a_dial::dial_message_streaming(home, member, params, on_delta, on_hitl, |_| {})
    });
    let home = mur_home.to_path_buf();
    let respond: RespondFn = Arc::new(move |member, id, allow| {
        if let Err(e) = crate::a2a_dial::dial_method(
            &home,
            member,
            "tool/hitl_respond",
            crate::cmd::agent::cli::stream::hitl_respond_params(&home, id, allow, HITL_SURFACE),
            crate::a2a_dial::DialMode::RequireRunning,
        ) {
            tracing::warn!(member, error = %e, "could not deliver the HITL answer");
        }
    });
    (dial, respond)
}

pub struct MurmurTransport {
    pub mur_home: PathBuf,
    pub req: Sender<DriverReq>,
    pub flags: ReviewFlags,
    // No reader in this crate and none in the plan's Tasks 8–12; the UI side
    // takes members from `WorkerHandle`. Remove or give it a reader in T12.
    #[allow(dead_code)]
    pub members: [String; 2],
    dial: DialFn,
    respond: RespondFn,
    last_committed: AtomicBool,
    pause: Mutex<Option<PauseKind>>,
    human_wait: Mutex<Duration>,
    /// Wait the loop drained via `take_human_wait` after a gate that asked
    /// to stop. The loop drops it on `Stopped`; the pause must still carry it.
    stopped_wait: Mutex<Duration>,
}

impl MurmurTransport {
    /// The real transport: A2A over `mur_home`.
    // Unused: `spawn_review_worker` builds the real edges itself and calls
    // `with_io`. Remove or route the worker through it in T12.
    #[allow(dead_code)]
    pub fn new(
        mur_home: PathBuf,
        req: Sender<DriverReq>,
        flags: ReviewFlags,
        members: [String; 2],
    ) -> Self {
        let (dial, respond) = real_io(&mur_home);
        Self::with_io(mur_home, req, flags, members, dial, respond)
    }

    /// Same, with the network edges injected.
    pub fn with_io(
        mur_home: PathBuf,
        req: Sender<DriverReq>,
        flags: ReviewFlags,
        members: [String; 2],
        dial: DialFn,
        respond: RespondFn,
    ) -> Self {
        Self {
            mur_home,
            req,
            flags,
            members,
            dial,
            respond,
            last_committed: AtomicBool::new(true),
            pause: Mutex::new(None),
            human_wait: Mutex::new(Duration::ZERO),
            stopped_wait: Mutex::new(Duration::ZERO),
        }
    }

    /// Why the last gate asked to stop, if it did: `User` (Esc×1) or
    /// `Detached` (the UI side is gone). The worker (T6) writes the pause.
    pub fn pause_kind(&self) -> Option<PauseKind> {
        *self.pause.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The UI side dropped its receiver: pause as `detached` (P3b-§4.4).
    /// Test probe: the worker reads `pause_kind` directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn detached(&self) -> bool {
        self.pause_kind() == Some(PauseKind::Detached)
    }

    fn set_pause(&self, kind: PauseKind) {
        *self.pause.lock().unwrap_or_else(|e| e.into_inner()) = Some(kind);
    }

    fn add_wait(&self, since: Instant) {
        *self.human_wait.lock().unwrap_or_else(|e| e.into_inner()) += since.elapsed();
    }

    /// Ask the UI one blocking question. `None` = the UI is gone, either
    /// when sending or while waiting for the answer.
    fn ask<T>(&self, build: impl FnOnce(std::sync::mpsc::SyncSender<T>) -> DriverReq) -> Option<T> {
        let (tx, rx) = sync_channel(1);
        self.req.send(build(tx)).ok()?;
        let started = Instant::now();
        let answer = rx.recv().ok();
        self.add_wait(started);
        answer
    }
}

impl ReviewTransport for MurmurTransport {
    fn send(&self, member: &str, params: &serde_json::Value) -> Result<String> {
        let cell = Arc::new(TurnCell::default());
        let slot = Arc::new(OnceLock::new());
        let _ = self.req.send(DriverReq::TurnStarted {
            member: member.to_string(),
            task_id: slot.clone(),
            turn: cell.clone(),
        });
        let mut streamed = String::new();
        let result = (self.dial)(
            &self.mur_home,
            member,
            params.clone(),
            &mut |delta, thinking, id| {
                if !thinking {
                    streamed.push_str(delta);
                }
                if !id.is_empty() {
                    let _ = slot.set(id.to_string());
                }
            },
            &mut |hitl| {
                answer_hitl(
                    member,
                    &hitl,
                    &|m, call| self.decide_hitl(m, call),
                    |id, allow| (self.respond)(member, id, allow),
                );
            },
        )
        .and_then(|task| task_reply(member, &task, streamed));
        // Settle the turn BEFORE telling the UI it ended (P3b-§6.3 step 4/6).
        self.last_committed.store(cell.commit(), Ordering::Release);
        let _ = self.req.send(DriverReq::TurnEnded {
            member: member.to_string(),
        });
        result
    }

    fn confirm_send(
        &self,
        member: &str,
        params: &serde_json::Value,
        open: &BTreeSet<String>,
    ) -> Result<SendAnswer> {
        if self.flags.detach_requested.load(Ordering::Acquire) {
            self.set_pause(PauseKind::Detached);
            return Ok(SendAnswer::Stop);
        }
        if self.flags.pause_requested.load(Ordering::Acquire) {
            self.set_pause(PauseKind::User);
            return Ok(SendAnswer::Stop);
        }
        let answer = self.ask(|reply| DriverReq::Confirm {
            member: member.to_string(),
            params: params.clone(),
            open: open.clone(),
            reply,
        });
        Ok(answer.unwrap_or_else(|| {
            self.set_pause(PauseKind::Detached);
            SendAnswer::Stop
        }))
    }

    fn ask_ruling(&self, pending: &EscalationRecord, ledger: &Ledger) -> Result<String> {
        let text = ruling_prompt_text(pending, ledger);
        let open = ledger.open_set().into_keys().collect();
        // A gone UI reads as an empty line, which leaves the session paused.
        Ok(self
            .ask(|reply| DriverReq::Ruling { text, open, reply })
            .unwrap_or_default())
    }

    fn show(&self, text: &str) -> Result<()> {
        let _ = self.req.send(DriverReq::Show(text.to_string()));
        Ok(())
    }

    fn take_human_wait(&self) -> Duration {
        let taken = std::mem::take(&mut *self.human_wait.lock().unwrap_or_else(|e| e.into_inner()));
        if self.pause_kind().is_some() {
            *self.stopped_wait.lock().unwrap_or_else(|e| e.into_inner()) += taken;
        }
        taken
    }

    fn take_requested_pause(&self) -> Option<RequestedPause> {
        let kind = self.pause_kind()?;
        let reason = match kind {
            PauseKind::Detached => REVIEW_PAUSE_REASON_DETACHED,
            _ => REVIEW_PAUSE_REASON_USER,
        };
        let drained = self.take_human_wait();
        let stopped =
            std::mem::take(&mut *self.stopped_wait.lock().unwrap_or_else(|e| e.into_inner()));
        Some(RequestedPause {
            kind,
            reason,
            human_wait: stopped + drained,
        })
    }

    fn turn_committed(&self) -> bool {
        self.last_committed.load(Ordering::Acquire)
    }
}

impl MurmurTransport {
    /// One gated call → one [`DriverReq::Hitl`]. A gone UI denies.
    fn decide_hitl(&self, member: &str, call: &serde_json::Value) -> bool {
        let Some(req) = HitlRequest::from_params(call.clone()).into_iter().next() else {
            return false;
        };
        self.ask(|reply| DriverReq::Hitl {
            member: member.to_string(),
            req,
            reply,
        })
        .unwrap_or(false)
    }
}
