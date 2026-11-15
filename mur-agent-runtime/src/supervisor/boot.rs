//! The synchronous half of the supervisor's boot, around the B1 seal: the
//! flags that print and exit, the hand-off to [`super::serve`], and the
//! runtime that is built only after the seal (GHSA-j8mv-27gx-x7xj).

use super::{seal, startup_override};
use crate::profile::Profile;
use anyhow::Context;
use mur_common::identity::AgentIdentity;
use std::path::PathBuf;
use std::sync::Arc;

/// Handle `--help` / `--version` / `--build-id` before any side effects
/// (process-group changes, socket creation, serve loop). Running the symlink
/// with these flags must print and exit, not silently start the supervisor.
/// `true` when one was printed and the caller should exit.
pub(super) fn print_meta_flag(argv: &[String]) -> bool {
    if crate::subcommand::has_flag(argv, &["--help", "-h"]) {
        let exe = argv
            .first()
            .map(String::as_str)
            .unwrap_or("mur-agent-runtime");
        println!(
            "{exe} — MUR per-agent A2A supervisor runtime\n\n\
             Usage: mur_agent_<name> [OPTIONS]\n\n\
             Running with no options starts the agent's A2A supervisor (Unix socket).\n\n\
             Options:\n  \
             --load <PATH>   Load and run a portable .muragent package\n  \
             -h, --help      Print this help and exit\n  \
             -V, --version   Print version and exit\n\n\
             Environment:\n  \
             MUR_HOME        Override the MUR home directory (default ~/.mur)"
        );
        return true;
    }
    if crate::subcommand::has_flag(argv, &["--version", "-V"]) {
        println!("mur-agent-runtime {}", env!("CARGO_PKG_VERSION"));
        return true;
    }
    if crate::subcommand::has_flag(argv, &["--build-id"]) {
        println!("{}", mur_common::build::SHORT_SHA);
        return true;
    }
    false
}

/// The multi-thread runtime the supervisor runs on. Built after the seal.
pub(super) fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")
}

/// What the synchronous boot hands to [`super::serve`].
pub(super) struct Boot {
    pub(super) mur_home: PathBuf,
    pub(super) agent_home: PathBuf,
    pub(super) profile: Profile,
    pub(super) identity: Arc<AgentIdentity>,
    pub(super) lock_path: PathBuf,
    pub(super) lock_handle: crate::lock_file::LockHandle,
    pub(super) pending_override: startup_override::Pending,
    pub(super) merged: Option<startup_override::Merged>,
    pub(super) boot_cfg: mur_common::config::Config,
    pub(super) boot_global_limits: mur_common::limits::Limits,
    pub(super) pre: seal::PreSeal,
}
