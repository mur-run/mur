//! Start the ssh-agent proxy (plan Task 2.3, O7 option A) around the seal:
//!
//! - [`start_private_before_seal`]: start a runtime-private ssh-agent holding
//!   the agent's dedicated key, while `secrets/` is still readable, and hold
//!   one connection to it. Every start path goes through here, including a
//!   service-manager start that runs no `mur` code. Only a key the profile
//!   grants. Failure warns; it never blocks start.
//! - [`connect_before_seal`]: the one upstream connection, made while the
//!   upstream socket is still reachable. Only when `entitlements.ssh` has
//!   keys. Failure warns; it never blocks start.
//! - [`start_after_seal`]: bind `<agent_home>/ssh-agent.sock` and serve.
//!   When the socket cannot exist (path too long for `sun_path`, bind
//!   failure) the proxy is off and the status file says why — there is no
//!   `/tmp` fallback.
//!
//! Windows never reaches here (O8): the proxy is unix-only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mur_common::agent::SshEntitlement;
use mur_common::ssh_proxy_status::{self, SshProxyState, SshProxyStatus};
use mur_ssh_request::keystore::{self, KeystoreError, StoredKey};

use super::private_agent::{PreSealDedicated, StartError};
use crate::ssh_proxy::gate::GateSlot;
use crate::ssh_proxy::listener::{self, DedicatedUpstream, HeldUpstream, Proxy};
use crate::ssh_proxy::policy::{PinTrust, Policy};
use crate::ssh_proxy::refusal::RefusalSink;

/// Status code: the proxy socket path does not fit `sun_path`.
pub(super) const OFF_SOCKET_PATH_TOO_LONG: &str = "socket_path_too_long";
/// Status code: binding the proxy socket failed.
pub(super) const OFF_SOCKET_BIND_FAILED: &str = "socket_bind_failed";
/// How a log line names the dedicated key when its rule has no label.
const DEDICATED_KEY_LABEL: &str = "dedicated key";

/// What [`start_private_before_seal`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DedicatedLoad {
    /// The profile grants no ssh key at all; the key store was not read.
    NoGrant,
    /// The agent has no dedicated key.
    NoKey,
    /// A key is stored but the profile does not grant it: not started.
    NotGranted,
    /// The private ssh-agent holds the key behind a held connection.
    Started,
    /// Reading the key or starting the private agent failed; already warned.
    Failed,
}

/// The outcome and, when [`DedicatedLoad::Started`], the private agent,
/// adopted into tokio after the seal.
pub(super) struct DedicatedStart {
    pub outcome: DedicatedLoad,
    pub pre: Option<PreSealDedicated>,
}

impl DedicatedStart {
    fn none(outcome: DedicatedLoad) -> Self {
        Self { outcome, pre: None }
    }
}

/// Start the runtime-private ssh-agent holding the agent's dedicated key,
/// before the seal (D spec §"Private agent lifecycle", plan Task 16.4).
///
/// The profile is the authority: a key file that appears in the directory is
/// never started unless `ent` grants its fingerprint. First, the key is
/// removed from the user's ssh-agent, where slice 6a loaded it (D spec
/// §"Clean-up"). Never gates start.
pub(super) fn start_private_before_seal(
    mur_home: &Path,
    agent: &str,
    ent: &SshEntitlement,
) -> DedicatedStart {
    // Runs on the main thread before the seal: catch a panic in the keystore
    // or the start so it still never gates start.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        start_private_with(
            mur_home,
            agent,
            ent,
            keystore::unload_from_user,
            super::private_agent::start,
        )
    }))
    .unwrap_or_else(|_| {
        tracing::warn!(
            "ssh-agent proxy: starting the private ssh-agent panicked; it was not started"
        );
        DedicatedStart::none(DedicatedLoad::Failed)
    })
}

/// Blocking on purpose: this runs before the tokio runtime exists (see
/// `supervisor::seal`), on the thread that is about to seal, so there is no
/// executor to stall. `stored` may run ssh-keygen, `unload` runs ssh-add.
fn start_private_with<U, S>(
    mur_home: &Path,
    agent: &str,
    ent: &SshEntitlement,
    unload: U,
    start: S,
) -> DedicatedStart
where
    U: FnOnce(&StoredKey) -> Result<bool, KeystoreError>,
    S: FnOnce(&Path, &StoredKey) -> Result<PreSealDedicated, StartError>,
{
    if ent.is_empty() {
        return DedicatedStart::none(DedicatedLoad::NoGrant);
    }
    let key = match keystore::stored(mur_home, agent) {
        Ok(Some(k)) => k,
        Ok(None) => return DedicatedStart::none(DedicatedLoad::NoKey),
        Err(e) => {
            // The path stays out of the log; `mur agent ssh` names it.
            let reason = match &e {
                KeystoreError::Unusable { reason, .. } => (*reason).to_string(),
                other => other.to_string(),
            };
            tracing::warn!(%reason, "ssh-agent proxy: the dedicated key is unusable and was not started");
            return DedicatedStart::none(DedicatedLoad::Failed);
        }
    };
    // Clean-up for every stored key, granted or not: 6a may have loaded it
    // while it was granted. Failure is never a reason to stop.
    if let Err(e) = unload(&key) {
        tracing::debug!(error = %e, "ssh-agent proxy: could not remove the dedicated key from the user's ssh-agent");
    }
    let Some(rule) = ent.keys.iter().find(|k| k.fingerprint == key.fingerprint) else {
        return DedicatedStart::none(DedicatedLoad::NotGranted);
    };
    let dir = match keystore::key_dir(mur_home, agent) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "ssh-agent proxy: the dedicated key directory is unusable");
            return DedicatedStart::none(DedicatedLoad::Failed);
        }
    };
    match start(&dir, &key) {
        Ok(pre) => DedicatedStart {
            outcome: DedicatedLoad::Started,
            pre: Some(pre),
        },
        Err(e) => {
            tracing::warn!(
                key = rule.label.as_deref().unwrap_or(DEDICATED_KEY_LABEL),
                error = %e,
                "ssh-agent proxy: could not start the private ssh-agent for the dedicated key; requests for it will be refused (upstream_unavailable)"
            );
            DedicatedStart::none(DedicatedLoad::Failed)
        }
    }
}

/// The pre-seal half of the upstream connection: a plain std stream, because
/// the runtime that will drive it does not exist yet. Adopted into a
/// [`HeldUpstream`] by [`PreSealUpstream::adopt`] after the seal.
pub(super) struct PreSealUpstream(Option<std::os::unix::net::UnixStream>);

impl PreSealUpstream {
    /// Hand the stream to tokio. Must run inside the runtime. A stream that
    /// cannot be registered degrades to disconnected, like a failed connect.
    pub(super) fn adopt(self) -> Arc<HeldUpstream> {
        let held = match self.0 {
            None => HeldUpstream::disconnected(),
            Some(s) => HeldUpstream::from_std(s).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "ssh-agent proxy: could not adopt the upstream connection; every ssh-agent request will be refused (upstream_unavailable)");
                HeldUpstream::disconnected()
            }),
        };
        Arc::new(held)
    }
}

/// Connect to the upstream agent now, before the seal. `None` when the agent
/// holds no ssh keys (no proxy at all). Disconnected when the upstream is
/// unset or unreachable: the proxy still runs and refuses
/// `upstream_unavailable`, which names the problem where the user looks.
pub(super) fn connect_before_seal(
    ent: &SshEntitlement,
    upstream: Option<&Path>,
) -> Option<PreSealUpstream> {
    if ent.is_empty() {
        return None;
    }
    let stream = match upstream {
        None => {
            tracing::warn!(
                "ssh-agent proxy: SSH_AUTH_SOCK is unset; every ssh-agent request will be refused (upstream_unavailable)"
            );
            None
        }
        Some(p) => match HeldUpstream::connect_blocking(p) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(
                    upstream = %p.display(),
                    error = %e,
                    "ssh-agent proxy: upstream ssh-agent unreachable; every ssh-agent request will be refused (upstream_unavailable)"
                );
                None
            }
        },
    };
    Some(PreSealUpstream(stream))
}

/// What the post-seal half needs.
pub(super) struct StartArgs<'a> {
    pub agent_home: &'a Path,
    pub agents_dir: PathBuf,
    pub ent: &'a SshEntitlement,
    pub pin_trust: PinTrust,
    pub upstream: Arc<HeldUpstream>,
    /// The runtime-private agent, when the dedicated key was started.
    pub dedicated: Option<Arc<DedicatedUpstream>>,
    pub sink: RefusalSink,
    pub marks: crate::ssh_proxy::mark::MarkSink,
    /// Filled by the supervisor once its approval channel exists; until then
    /// every sign request is refused `hitl_unavailable`.
    pub approvals: Arc<GateSlot>,
}

/// Bind and serve. Returns the socket path children get as `SSH_AUTH_SOCK`,
/// or `None` when the proxy is off (recorded in the status file).
pub(super) async fn start_after_seal(a: StartArgs<'_>) -> Option<PathBuf> {
    let path = listener::socket_path(a.agent_home);
    if !listener::socket_path_fits(&path) {
        record_off(
            a.agent_home,
            OFF_SOCKET_PATH_TOO_LONG,
            format!(
                "the proxy socket path {} is too long for a unix socket, so the proxy \
                 is not started and SSH_AUTH_SOCK is unset (MUR never falls back to /tmp)",
                path.display()
            ),
        );
        return None;
    }
    let l = match listener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            record_off(
                a.agent_home,
                OFF_SOCKET_BIND_FAILED,
                format!(
                    "could not listen on {} ({e}), so the proxy is not started and \
                     SSH_AUTH_SOCK is unset",
                    path.display()
                ),
            );
            return None;
        }
    };
    let policy = Policy::build(a.ent, a.pin_trust, Policy::UNPINNED_IS_UNTRUSTED);
    warn_at_start(&policy, &a.upstream, a.dedicated.as_deref()).await;
    let proxy = Arc::new(Proxy {
        policy,
        upstream: a.upstream,
        dedicated: a.dedicated,
        sink: a.sink,
        marks: a.marks,
        gate: listener::self_only_gate(std::process::id(), a.agents_dir),
        approvals: a.approvals,
    });
    tokio::spawn(listener::serve(l, proxy));
    tracing::info!(socket = %path.display(), "ssh-agent proxy listening");
    Some(path)
}

/// Start-time warnings. Neither blocks start.
async fn warn_at_start(
    policy: &Policy,
    upstream: &HeldUpstream,
    dedicated: Option<&DedicatedUpstream>,
) {
    if let Policy::RefuseAll(code) = policy {
        tracing::warn!(
            code = code.as_str(),
            "ssh-agent proxy: every signature will be refused"
        );
        return;
    }
    if !upstream.is_connected().await {
        return; // already warned at connect time
    }
    match listener::missing_upstream_keys(policy, upstream, dedicated).await {
        Some(missing) => {
            for fp in missing {
                tracing::warn!(key = %fp, "ssh-agent proxy: key listed in entitlements.ssh is not held by the upstream ssh-agent");
            }
        }
        None => tracing::warn!(
            "ssh-agent proxy: could not list the upstream ssh-agent's keys; requests will be refused (upstream_unavailable)"
        ),
    }
}

fn record_off(agent_home: &Path, code: &str, explanation: String) {
    let status = off_status(code, explanation);
    tracing::warn!(code, "ssh-agent proxy is off: {}", status.explanation);
    if let Err(e) = ssh_proxy_status::write(agent_home, &status) {
        tracing::warn!(error = %e, "could not record the ssh-agent proxy status");
    }
}

pub(super) fn off_status(code: &str, explanation: String) -> SshProxyStatus {
    SshProxyStatus {
        state: SshProxyState::Off,
        code: Some(code.to_string()),
        explanation,
        at_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
    }
}

#[cfg(test)]
#[path = "ssh_proxy_boot_tests.rs"]
mod tests;
