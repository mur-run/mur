//! `mur agent start|restart <name>` with a startup-override flag: one
//! orchestrator shared by the CLI and the Hub. It writes the
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
mod normal;
mod outcome;
mod prepare;
mod quit;
pub(super) mod run;
#[cfg(test)]
mod test_support;
mod writer;

pub use confirm::banner;
pub(super) use confirm::{Confirmation, Terminal, confirm};
pub use normal::{NormalRestart, restart_normally};
pub use outcome::{Cleanup, OverrideOutcome, Step};
pub use prepare::prepare;
pub use quit::withdraw_on_quit;
pub use run::override_restart;

/// Which command the override rides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The agent is running: stop it, the next boot takes the file.
    Restart,
    /// The agent is stopped: start it; there is nothing to stop. CLI only.
    Start,
}

/// Who is writing, and how the human confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issuer {
    Cli {
        confirmed_by: ConfirmedBy,
    },
    /// The Hub's native dialog was accepted in `window_label`. The Hub builds
    /// this only from its own confirmation type (Hub spec §Command, step 8).
    Hub {
        window_label: String,
    },
}

impl Issuer {
    fn issued_by(&self) -> IssuedBy {
        match self {
            Self::Cli { .. } => IssuedBy::Cli,
            Self::Hub { .. } => IssuedBy::Hub,
        }
    }

    fn confirmed_by(&self) -> ConfirmedBy {
        match self {
            Self::Cli { confirmed_by } => *confirmed_by,
            Self::Hub { .. } => ConfirmedBy::HubNativeDialog,
        }
    }

    /// The Hub window label for audit lines; `None` for the CLI.
    fn window(&self) -> Option<String> {
        match self {
            Self::Cli { .. } => None,
            Self::Hub { window_label } => Some(window_label.clone()),
        }
    }
}

pub struct OverrideRequest<'a> {
    /// Resolved once by the caller; the home the respawned runtime reads.
    pub home: &'a std::path::Path,
    /// A validated agent slug.
    pub name: &'a str,
    pub mode: Mode,
    pub flags: OverrideFlags,
    /// Who asks, for refusal wording only ("this CLI" / "this Hub").
    pub by: IssuedBy,
}

/// Checks passed and the writer lock is held. Owned, so it can move to
/// another thread; dropping it anywhere releases the lock.
#[derive(Debug)]
pub struct Prepared {
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
    pub fn has_service(&self) -> bool {
        self.has_service
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn home(&self) -> &std::path::Path {
        &self.home
    }

    /// The profile's stop timeout, for a launcher's `kill_wait`.
    pub fn stop_timeout_secs(&self) -> u64 {
        self.stop_timeout_secs
    }
}

/// Lets the Hub refuse a publish while it is quitting, and take over the
/// claim. Per agent, so no method takes a name.
pub trait JobGate: Send + 'static {
    /// Under the writer lock, just before publish. `None`: abort, nothing
    /// published (`Step::Cancelled`).
    fn begin_publish(&mut self) -> Option<Box<dyn PublishPermit>>;
    /// Before Cleanup or the final classify. `false`: the quit path owns the
    /// claim; return `Abandoned` and touch nothing.
    fn begin_cleanup(&mut self) -> bool;
}

pub trait PublishPermit: Send {
    /// Publish succeeded.
    fn commit(self: Box<Self>, nonce: &str);
}

/// The CLI's gate: never quits.
pub struct AlwaysYes;

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
pub const NOTHING_CHANGED: &str = "nothing was changed";
