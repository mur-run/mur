//! Channel consult C4 (§D4, *Scope carriage*): the asker's read scope for one
//! consult turn.
//!
//! The `read_file` tool and the project-instructions gate are built once per
//! runner and shared by every turn it runs, so narrowing their policy for a
//! consult would leak one consult's restriction into an ordinary turn beside
//! it, or one asker's scope into another asker's consult. Instead the asker's
//! effective policy travels on the turn's [`TaskSpec`](super::TaskSpec) as an
//! immutable [`ConsultScope`], and `run_sync` binds it as a tokio task-local
//! around the turn's generation future — the same pattern, for the same
//! reason, as the per-turn model ledger ([`super::turn_models`]).
//!
//! The binding lives exactly as long as the future: completion, failure and
//! `tasks/cancel` all drop it, so there is nothing to restore and nothing to
//! leak into the next turn (R10).
//!
//! A tokio task-local does not cross `tokio::spawn`. Every read entry point of
//! a consult turn — `read_file` (through `GuardedToolCall::execute_scoped`)
//! and the project-instructions loader (through `pinned_and_prior`) — runs on
//! the generation future itself, and tests pin both. A read entry point added
//! later that runs on a spawned task must take the scope as an argument.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use mur_common::agent::FilesystemEntitlement;

use crate::sandbox::launch_chain::LaunchChain;

/// The asking agent's effective read policy, as its own file tools would
/// enforce it (§D3.6). Immutable once built: there is no setter, so a consult
/// turn cannot widen or swap the scope it runs under.
#[derive(Debug)]
pub struct ConsultScope {
    asker: String,
    fs: FilesystemEntitlement,
    chain: LaunchChain,
}

impl ConsultScope {
    /// The scope for `asker`, read from its own `profile.yaml` under
    /// `asker_home` through [`file_tool_policy_of`] — the constructor its
    /// own file tools are built with. Nothing the caller holds contributes to
    /// it; an unreadable or unparsable profile is an error, never an empty or
    /// default scope (R4).
    ///
    /// [`file_tool_policy_of`]: crate::tools::fs_policy::file_tool_policy_of
    pub(crate) fn of(
        asker: impl Into<String>,
        asker_home: &Path,
    ) -> Result<Self, crate::profile::ProfileLoadError> {
        let (fs, chain) = crate::tools::fs_policy::file_tool_policy_of(asker_home)?;
        Ok(Self::new(asker, fs, chain))
    }

    /// From an already-effective policy. Callers outside tests go through
    /// [`Self::of`]; a raw profile entitlement must never be passed here.
    pub(crate) fn new(
        asker: impl Into<String>,
        fs: FilesystemEntitlement,
        chain: LaunchChain,
    ) -> Self {
        Self {
            asker: asker.into(),
            fs,
            chain,
        }
    }

    /// The asking agent's id, for refusal messages.
    pub fn asker(&self) -> &str {
        &self.asker
    }

    pub(crate) fn fs(&self) -> &FilesystemEntitlement {
        &self.fs
    }

    pub(crate) fn chain(&self) -> &LaunchChain {
        &self.chain
    }
}

tokio::task_local! {
    /// The consult scope of the turn whose generation future is being polled.
    static CURRENT: Arc<ConsultScope>;
}

/// Run `fut` (one turn's generation) under `scope`. `None` — every ordinary
/// turn — runs it unchanged, with no binding at all.
pub(crate) async fn scope<F: Future>(scope: Option<Arc<ConsultScope>>, fut: F) -> F::Output {
    match scope {
        Some(s) => CURRENT.scope(s, fut).await,
        None => fut.await,
    }
}

/// The consult scope bound to the current turn, if any.
pub(crate) fn current() -> Option<Arc<ConsultScope>> {
    CURRENT.try_with(Arc::clone).ok()
}

/// Synchronous twin of [`scope`], for gate-level tests.
#[cfg(test)]
pub(crate) fn sync_scope<R>(scope: Arc<ConsultScope>, f: impl FnOnce() -> R) -> R {
    CURRENT.sync_scope(scope, f)
}
