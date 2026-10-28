//! Recoverable deletes for agent `rm` (issue #2154).
//!
//! Lives below both `mur-core` and `mur-agent-runtime`: the runtime's `bash`
//! tool, the daemon, and the CLI all move things into the same per-agent
//! trash, and the runtime must not pull in `mur-core`.

pub mod detect;
pub mod expire;
pub mod layout;
pub mod mover;
pub mod publish;
pub mod safety;
pub mod trash;

#[cfg(all(test, unix))]
mod expire_tests;
#[cfg(all(test, unix))]
mod move_tests;
#[cfg(all(test, unix))]
mod publish_tests;
#[cfg(all(test, unix))]
mod trash_tests;

pub use detect::{RmPlan, Rule, TrashTarget, plan};
pub use expire::{
    EXPIRY_CURSOR_FILE, ExpireReport, expire_due, expiry_cursor_path, lookup_days, retained_bytes,
};
pub use layout::{entry_path, ledger_dir, staging_root, sweep_partials, trash_root};
pub use mover::{MoveOutcome, move_across, move_staged, move_with, move_with_staging};
pub use publish::rename_noreplace;
pub use safety::{Refusal, check_target, remove_within, resolve_no_follow};
pub use trash::{TrashOutcome, record_unprotected, trash_into, tree_size};
