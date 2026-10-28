//! `mur agent start|restart <name>` with a startup-override flag: one
//! orchestrator shared by the CLI and (later) the Hub. It writes the
//! one-shot override file, launches the boot, and reports what that boot did
//! with it.
//!
//! Specs: `2026-10-25-startup-permission-overrides-design.md` (§When the file
//! is written, §Acknowledgement) and `2026-10-25-startup-overrides-hub-design.md`
//! (§Orchestrator).

use std::path::PathBuf;

use mur_common::agent::OverrideFlags;
use mur_common::startup_override::{ConfirmedBy, IssuedBy};

pub(super) mod audit;
mod confirm;
mod outcome;
mod prepare;
pub(super) mod run;
mod writer;

pub(super) use confirm::{Confirmation, Terminal, confirm};
pub(super) use outcome::{Cleanup, OverrideOutcome, Step};
pub(super) use prepare::prepare;
pub(super) use run::override_restart;

/// Which command the override rides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// The agent is running: stop it, the next boot takes the file.
    Restart,
    /// The agent is stopped: start it; there is nothing to stop. CLI only.
    Start,
}

/// Who is writing, and how the human confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Issuer {
    Cli { confirmed_by: ConfirmedBy },
    // `Hub { window_label }` lands with the Hub PR.
}

impl Issuer {
    fn issued_by(&self) -> IssuedBy {
        match self {
            Self::Cli { .. } => IssuedBy::Cli,
        }
    }

    fn confirmed_by(&self) -> ConfirmedBy {
        match self {
            Self::Cli { confirmed_by } => *confirmed_by,
        }
    }

    /// The Hub window label for audit lines; `None` for the CLI.
    fn window(&self) -> Option<String> {
        match self {
            Self::Cli { .. } => None,
        }
    }
}

pub(super) struct OverrideRequest<'a> {
    /// Resolved once by the caller; the home the respawned runtime reads.
    pub home: &'a std::path::Path,
    /// A validated agent slug.
    pub name: &'a str,
    pub mode: Mode,
    pub flags: OverrideFlags,
}

/// Checks passed and the writer lock is held. Owned, so it can move to
/// another thread; dropping it anywhere releases the lock.
#[derive(Debug)]
pub(super) struct Prepared {
    home: PathBuf,
    name: String,
    mode: Mode,
    flags: OverrideFlags,
    has_service: bool,
    /// `Mode::Restart`: the pid the running lock names.
    old_pid: Option<u32>,
    stop_timeout_secs: u64,
    _lock: writer::WriterLock,
}

impl Prepared {
    pub(super) fn has_service(&self) -> bool {
        self.has_service
    }
}

/// Lets the Hub refuse a publish while it is quitting, and take over the
/// claim. Per agent, so no method takes a name.
pub(super) trait JobGate: Send + 'static {
    /// Under the writer lock, just before publish. `None`: abort, nothing
    /// published (`Step::Cancelled`).
    fn begin_publish(&mut self) -> Option<Box<dyn PublishPermit>>;
    /// Before Cleanup or the final classify. `false`: the quit path owns the
    /// claim; return `Abandoned` and touch nothing.
    fn begin_cleanup(&mut self) -> bool;
}

pub(super) trait PublishPermit: Send {
    /// Publish succeeded.
    fn commit(self: Box<Self>, nonce: &str);
}

/// The CLI's gate: never quits.
pub(super) struct AlwaysYes;

struct NoopPermit;

impl PublishPermit for NoopPermit {
    fn commit(self: Box<Self>, _nonce: &str) {}
}

impl JobGate for AlwaysYes {
    fn begin_publish(&mut self) -> Option<Box<dyn PublishPermit>> {
        Some(Box::new(NoopPermit))
    }

    fn begin_cleanup(&mut self) -> bool {
        true
    }
}

/// Every refusal says nothing was written; the wording is shared.
pub(super) const NOTHING_CHANGED: &str = "nothing was changed";
