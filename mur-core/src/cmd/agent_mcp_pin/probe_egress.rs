//! Run a live MCP probe the way the runtime would spawn the server (#1639,
//! #1647): a `Restricted` / `BroadAudited` entry is started behind a loopback
//! egress proxy with a tokened `HTTPS_PROXY`, using the runtime's own predicate
//! (`mur_common::agent::entries_need_egress`). Every probe call site goes
//! through here so none of them can quietly fall back to "no proxy" again.

use super::{ProbeError, probe_mcp_descriptions};
use mur_agent_runtime::protocol::mcp_client::ToolInfo;
use mur_agent_runtime::sandbox::policy::SandboxPolicy;
use mur_common::agent::McpServerEntry;

/// Probe `entry` as `agent` would spawn it, and tear the probe's proxy down
/// before returning — on success and on failure alike.
///
/// Synchronous and self-contained on purpose. The probe runs on its own
/// thread with its own current-thread runtime instead of the caller's:
/// `Handle::current()` panics outside a runtime and `block_in_place` panics on
/// a current_thread one, and the callers span all three (CLI, a synchronous
/// Tauri command, unit tests). It blocks the calling thread for at most one
/// probe timeout, which every caller was already doing.
///
/// ⚠ LIFETIME: the proxy's accept loop is a task on the probe-owned runtime,
/// and the ONLY thing that stops it is that runtime being dropped when the
/// thread returns. That runtime IS the proxy's RAII guard. Do NOT start the
/// proxy on the caller's runtime instead — it would keep listening on
/// 127.0.0.1 for the rest of the caller's process, long after the probe
/// returned. The `assert_port_closed` checks in `mcp_add_proxy_tests.rs` and
/// `agent_mcp_pin/tests.rs` fail if this guarantee breaks.
pub fn probe_as_runtime_would(
    agent: &str,
    entry: &McpServerEntry,
    timeout: std::time::Duration,
    policy: &SandboxPolicy,
) -> Result<(String, Vec<ToolInfo>), ProbeError> {
    std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| ProbeError::Setup(format!("build probe runtime: {e}")))?;
            if needs_browser_key(entry) {
                cache_browser_key(timeout);
            }
            rt.block_on(async {
                let proxy = if mur_common::agent::entries_need_egress(std::slice::from_ref(entry)) {
                    Some(
                        mur_agent_runtime::sandbox::egress_proxy::start_egress_proxy(agent)
                            .await
                            .map_err(|e| {
                                ProbeError::Setup(format!("start probe egress proxy: {e}"))
                            })?,
                    )
                } else {
                    None
                };
                probe_mcp_descriptions(entry, timeout, policy, proxy.as_ref()).await
            })
        })
        .join()
        .unwrap_or_else(|_| Err(ProbeError::Setup("probe thread panicked".into())))
    })
}

/// Whether the probe must read the browser state key before spawning `entry`:
/// the same live-browser-with-a-profile shape the runtime hands the key to
/// (`mcp_client::state_key`). Without it the sandboxed child falls back to
/// the Keychain it cannot reach, so the probe tests a configuration the
/// runtime never runs (#2468).
fn needs_browser_key(entry: &McpServerEntry) -> bool {
    mur_common::browser_state_key::wants_state_key(&entry.args)
}

/// Read the key into the pre-seal cache, mirroring the runtime's
/// `supervisor::browser_key::cache_before_seal`. Bounded on its own thread:
/// an unauthorised Keychain item raises a modal prompt that blocks until
/// clicked, and the probe must not outlive its own timeout over it. Never
/// fails the probe — a miss leaves the child on the Keychain fallback, which
/// is exactly what the probe did before.
fn cache_browser_key(timeout: std::time::Duration) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = mur_common::browser_state_key::keychain_ref();
        let _ = tx.send(mur_common::secret::cache_before_seal(&r));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(())) => tracing::debug!("probe: browser state key resolved before spawn"),
        Ok(Err(e)) => tracing::debug!(error = %e, "probe: browser state key not available"),
        Err(_) => tracing::warn!(
            timeout_secs = timeout.as_secs(),
            "probe: keychain did not answer for the browser state key in time"
        ),
    }
}

#[cfg(test)]
mod browser_key_tests {
    use super::needs_browser_key;
    use mur_common::agent::McpServerEntry;

    fn entry(args: &[&str]) -> McpServerEntry {
        McpServerEntry {
            name: "browser".into(),
            command: "mur".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    /// The probe must read the browser key before spawning a live browser,
    /// exactly as the supervisor does before its seal (#2468) — or it probes
    /// a configuration the runtime never runs.
    #[test]
    fn a_live_browser_probe_reads_the_key_first_and_nothing_else_does() {
        assert!(needs_browser_key(&entry(&[
            "browser",
            "record",
            "--mode",
            "live",
            "--profile",
            "g"
        ])));
        assert!(!needs_browser_key(&entry(&["serve"])));
    }
}
