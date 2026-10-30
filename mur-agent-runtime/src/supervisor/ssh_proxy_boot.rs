//! Start the ssh-agent proxy (plan Task 2.3, O7 option A) in two halves
//! around the seal:
//!
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

use crate::ssh_proxy::gate::GateSlot;
use crate::ssh_proxy::listener::{self, HeldUpstream, Proxy};
use crate::ssh_proxy::policy::{PinTrust, Policy};
use crate::ssh_proxy::refusal::RefusalSink;

/// Status code: the proxy socket path does not fit `sun_path`.
pub(super) const OFF_SOCKET_PATH_TOO_LONG: &str = "socket_path_too_long";
/// Status code: binding the proxy socket failed.
pub(super) const OFF_SOCKET_BIND_FAILED: &str = "socket_bind_failed";

/// Connect to the upstream agent now, before the seal. `None` when the agent
/// holds no ssh keys (no proxy at all). `Some(disconnected)` when the
/// upstream is unset or unreachable: the proxy still runs and refuses
/// `upstream_unavailable`, which names the problem where the user looks.
pub(super) async fn connect_before_seal(
    ent: &SshEntitlement,
    upstream: Option<&Path>,
) -> Option<Arc<HeldUpstream>> {
    if ent.is_empty() {
        return None;
    }
    let held = match upstream {
        None => {
            tracing::warn!(
                "ssh-agent proxy: SSH_AUTH_SOCK is unset; every ssh-agent request will be refused (upstream_unavailable)"
            );
            HeldUpstream::disconnected()
        }
        Some(p) => match HeldUpstream::connect(p).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(
                    upstream = %p.display(),
                    error = %e,
                    "ssh-agent proxy: upstream ssh-agent unreachable; every ssh-agent request will be refused (upstream_unavailable)"
                );
                HeldUpstream::disconnected()
            }
        },
    };
    Some(Arc::new(held))
}

/// What the post-seal half needs.
pub(super) struct StartArgs<'a> {
    pub agent_home: &'a Path,
    pub agents_dir: PathBuf,
    pub ent: &'a SshEntitlement,
    pub pin_trust: PinTrust,
    pub upstream: Arc<HeldUpstream>,
    pub sink: RefusalSink,
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
    warn_at_start(&policy, &a.upstream).await;
    let proxy = Arc::new(Proxy {
        policy,
        upstream: a.upstream,
        sink: a.sink,
        gate: listener::self_only_gate(std::process::id(), a.agents_dir),
        approvals: a.approvals,
    });
    tokio::spawn(listener::serve(l, proxy));
    tracing::info!(socket = %path.display(), "ssh-agent proxy listening");
    Some(path)
}

/// Start-time warnings. Neither blocks start.
async fn warn_at_start(policy: &Policy, upstream: &HeldUpstream) {
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
    match listener::missing_upstream_keys(policy, upstream).await {
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
