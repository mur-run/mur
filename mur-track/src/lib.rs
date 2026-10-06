//! Copy-on-write tracks for agent work.
//!
//! One `ParallelBackend` seam, several substrates: ZFS native (Linux/FreeBSD),
//! ZFS over a `mur-zfs-agent` socket (OrbStack / Lima / WSL2 VM), and git
//! worktrees (always available). `detect_backend` picks the best one for a
//! project.
//!
//! Lives below `mur-core` so that `mur-agent-runtime` can depend on it without
//! pulling LanceDB + Arrow into every agent process.

pub mod backend;
pub mod snapshot_request;
pub mod turn;
pub mod zfs_protocol;

pub use backend::{
    GitWorktreeBackend, ParallelBackend, ZfsNativeBackend, ZfsSocketBackend, detect_backend,
};
pub use turn::{TreeClone, TurnTrack};
