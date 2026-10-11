//! Types only (P3b-§4.1, §10): what the review worker asks the UI thread, and
//! what it reports when it ends. The worker blocks on each `reply`; nothing
//! here is async.

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, OnceLock};

use crate::cmd::agent::cli::stream::HitlRequest;

use super::super::driver::SendAnswer;
use super::super::ledger::Ledger;
use super::super::loop_driver::LoopDriverStop;
use super::super::turn_cell::TurnCell;

/// A one-shot reply slot: the worker `recv`s on the other end. A dropped
/// sender reads as "the human is gone" on the worker side.
pub type Reply<T> = SyncSender<T>;

/// Worker → UI thread.
#[derive(Debug)]
pub enum DriverReq {
    /// The send gate (P2-§5.3). `open` is the open set a `/rule` line is
    /// validated against.
    Confirm {
        member: String,
        params: serde_json::Value,
        open: BTreeSet<String>,
        reply: Reply<SendAnswer>,
    },
    /// An escalation needs a ruling. `text` is the shown block; `open` is the
    /// open set. The reply is the raw line, `""` on abort.
    Ruling {
        text: String,
        open: BTreeSet<String>,
        reply: Reply<String>,
    },
    /// One line or block for the transcript.
    Show(String),
    /// A member turn began. `task_id` is filled once the runtime names the
    /// task; `turn` is the abort / commit cell for this turn alone.
    TurnStarted {
        member: String,
        task_id: Arc<OnceLock<String>>,
        turn: Arc<TurnCell>,
    },
    /// The turn's reply is back (or failed); the cell is already settled.
    TurnEnded {
        // No reader: one turn is in flight at a time, so the UI clears it
        // unconditionally. Kept so a log of `DriverReq`s names the member.
        #[allow(dead_code)]
        member: String,
    },
    /// A gated tool call raised inside a member's turn: one request per call
    /// (#1759). `reply` is allow / deny.
    Hitl {
        member: String,
        req: HitlRequest,
        reply: Reply<bool>,
    },
}

/// How the worker ended.
#[derive(Debug)]
pub enum Outcome {
    /// The loop ran to a stop; the ledger and channel id feed the stop
    /// screen. The ledger is boxed: it dwarfs every other variant.
    Ran(LoopDriverStop, Box<Ledger>, String),
    /// The human chose to leave a paused session paused.
    LeftPaused,
    Err(String),
}

/// Worker → UI thread, once, last.
#[derive(Debug)]
pub enum DriverEvent {
    Finished(Outcome),
}

/// Flags the UI thread sets and the worker reads. Read only after a send
/// returns (P3b-§4.2). There is no abort flag: abort lives in the per-turn
/// [`TurnCell`] (D7).
#[derive(Clone, Default)]
pub struct ReviewFlags {
    pub pause_requested: Arc<AtomicBool>,
    /// MURMUR is closing (§4.4.1): pause as `detached`, not `user`.
    pub detach_requested: Arc<AtomicBool>,
}

/// Who answers a tool approval raised during a review turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitlOrigin {
    /// The attached agent's own turn.
    Own,
    /// A review member's turn: Once / Deny only, and the worker responds.
    Review,
}
