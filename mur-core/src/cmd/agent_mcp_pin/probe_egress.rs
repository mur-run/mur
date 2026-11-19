//! Run a live MCP probe the way the runtime would spawn the server (#1639,
//! #1647): a `Restricted` / `BroadAudited` entry is started behind a loopback
//! egress proxy with a tokened `HTTPS_PROXY`, using the runtime's own predicate
//! (`mur_common::agent::entries_need_egress`). Every probe call site goes
//! through here so none of them can quietly fall back to "no proxy" again.

use super::{ProbeError, probe_mcp_descriptions};
use mur_agent_runtime::protocol::mcp_client::ToolInfo;
use mur_agent_runtime::sandbox::policy::SandboxPolicy;
use mur_common::agent::McpServerEntry;
use std::future::Future;
use std::time::{Duration, Instant};

/// Probe `entry` as `agent` would spawn it, and tear the probe's proxy down
/// before returning — on success and on failure alike.
///
/// Synchronous and self-contained on purpose. The probe runs on its own
/// thread with its own current-thread runtime instead of the caller's:
/// `Handle::current()` panics outside a runtime and `block_in_place` panics on
/// a current_thread one, and the callers span all three (CLI, a synchronous
/// Tauri command, unit tests). It blocks the calling thread for at most one
/// probe timeout in total — key resolution included — which every caller was
/// already doing.
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
    timeout: Duration,
    policy: &SandboxPolicy,
) -> Result<(String, Vec<ToolInfo>), ProbeError> {
    let resolve_key = needs_browser_key(entry).then_some(cache_browser_key);
    run_within_budget(timeout, resolve_key, |deadline| async move {
        let proxy = if mur_common::agent::entries_need_egress(std::slice::from_ref(entry)) {
            Some(
                mur_agent_runtime::sandbox::egress_proxy::start_egress_proxy(agent)
                    .await
                    .map_err(|e| ProbeError::Setup(format!("start probe egress proxy: {e}")))?,
            )
        } else {
            None
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        probe_mcp_descriptions(entry, remaining, policy, proxy.as_ref()).await
    })
}

/// Run `probe` on its own thread and current-thread runtime (see
/// `probe_as_runtime_would` for why), with ONE deadline shared by the
/// optional pre-spawn key resolution and the probe itself: `probe` gets the
/// deadline, not a fresh timeout, so a slow Keychain plus a stalled handshake
/// still stops at `timeout` overall. A resolver that eats the whole budget
/// ends the probe with `ProbeError::Timeout` before anything is spawned.
fn run_within_budget<T, R, F, Fut>(
    timeout: Duration,
    resolve_key: Option<R>,
    probe: F,
) -> Result<T, ProbeError>
where
    T: Send,
    R: FnOnce() + Send + 'static,
    F: FnOnce(Instant) -> Fut + Send,
    Fut: Future<Output = Result<T, ProbeError>>,
{
    let deadline = Instant::now() + timeout;
    std::thread::scope(|s| {
        s.spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| ProbeError::Setup(format!("build probe runtime: {e}")))?;
            if let Some(resolve) = resolve_key
                && !resolve_before(resolve, deadline)
            {
                return Err(ProbeError::Timeout(timeout));
            }
            // Report the configured budget, not the remainder the probe saw.
            match rt.block_on(probe(deadline)) {
                Err(ProbeError::Timeout(_)) => Err(ProbeError::Timeout(timeout)),
                other => other,
            }
        })
        .join()
        .unwrap_or_else(|_| Err(ProbeError::Setup("probe thread panicked".into())))
    })
}

/// Run `resolve` on a detached thread and wait for it until `deadline`.
/// Detached because an unauthorised Keychain item raises a modal prompt that
/// blocks until clicked; the probe must not outlive its budget over it.
/// Returns false when the deadline passed first.
fn resolve_before(resolve: impl FnOnce() + Send + 'static, deadline: Instant) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        resolve();
        let _ = tx.send(());
    });
    let waited = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    if waited.is_err() {
        tracing::warn!("probe: keychain did not answer for the browser state key in time");
    }
    waited.is_ok()
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
/// `supervisor::browser_key::cache_before_seal`. A miss never fails the
/// probe — it leaves the child on the Keychain fallback, which is exactly
/// what the probe did before. Bounded by the caller (`resolve_before`).
fn cache_browser_key() {
    let r = mur_common::browser_state_key::keychain_ref();
    match mur_common::secret::cache_before_seal(&r) {
        Ok(()) => tracing::debug!("probe: browser state key resolved before spawn"),
        Err(e) => tracing::debug!(error = %e, "probe: browser state key not available"),
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

    use super::run_within_budget;
    use crate::cmd::agent_mcp_pin::ProbeError;
    use std::time::{Duration, Instant};

    const BUDGET: Duration = Duration::from_millis(1200);

    /// A slow Keychain and a stalled handshake must share ONE budget: the
    /// handshake gets what the key lookup left, not a fresh full timeout
    /// (which made a T-second probe block for ~2T).
    #[test]
    fn a_slow_key_and_a_stalled_handshake_share_one_timeout() {
        let start = Instant::now();
        let r: Result<(), _> = run_within_budget(
            BUDGET,
            Some(|| std::thread::sleep(BUDGET * 3 / 4)),
            |deadline| async move {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    remaining < BUDGET / 2,
                    "handshake got a fresh budget: {remaining:?}"
                );
                tokio::time::sleep(remaining).await; // a handshake that never answers
                Err(ProbeError::Timeout(remaining))
            },
        );
        assert!(
            matches!(r, Err(ProbeError::Timeout(t)) if t == BUDGET),
            "{r:?}"
        );
        assert!(
            start.elapsed() < BUDGET * 3 / 2,
            "took {:?}",
            start.elapsed()
        );
    }

    /// A key lookup that outlasts the whole budget ends the probe as a
    /// timeout without spawning the server.
    #[test]
    fn a_key_lookup_past_the_budget_times_out_before_spawning() {
        let start = Instant::now();
        let r: Result<(), _> =
            run_within_budget(BUDGET, Some(|| std::thread::sleep(BUDGET * 4)), |_| async {
                panic!("probe must not run once the budget is spent")
            });
        assert!(
            matches!(r, Err(ProbeError::Timeout(t)) if t == BUDGET),
            "{r:?}"
        );
        assert!(
            start.elapsed() < BUDGET * 3 / 2,
            "took {:?}",
            start.elapsed()
        );
    }
}
