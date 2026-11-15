//! Agent runtime entrypoint — assembles profile, dispatcher, telemetry, and
//! drives the stdio (and optionally Unix-socket) transports until SIGTERM.

use crate::entitlements::detect_warnings;
use crate::multi_call::{DispatchError, extract_profile_name, verify_name_match};
use crate::profile::Profile;
use crate::protocol::a2a_server::Dispatcher;
use crate::protocol::methods::{
    card::CardHandler,
    message_send::MessageSendHandler,
    tasks::{TasksCancelHandler, TasksGetHandler, TasksListHandler},
};
use crate::task_runner::TaskRunner;
use crate::telemetry_writer::TelemetryWriter;
use mur_common::agent::LockTransports;
use mur_common::identity::AgentIdentity;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tracing::{info, warn};

/// Time for cancelled turns to settle their tracks. With the default
/// `stop_timeout_secs` and `2 x SHUTDOWN_JOB_KILL_GRACE` it sizes the fixed
/// 19 s stop budget (`stop_budget::STOP_BUDGET`), which leaves ~1s of
/// teardown under launchd's default 20s `ExitTimeOut`; inside it the MCP
/// stop has a reserved slice and the drain gets what is left (#2360).
const SHUTDOWN_CANCEL_GRACE_SECS: u64 = 3;
/// SIGTERM-to-SIGKILL grace for the bash jobs still alive after the drain.
/// Turn-owned jobs already had their SIGTERM and the drain's grace; this is
/// the last sweep, kept short so a SIGTERM-ignoring job cannot push the stop
/// past the service manager's budget (#1909).
const SHUTDOWN_JOB_KILL_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

mod boot;
mod bootstrap;
mod browser_key;
mod dispatch;
mod identity;
mod pin;
#[cfg(unix)]
mod private_agent;
mod seal;
mod serve;
mod ssh_gate_boot;
#[cfg(unix)]
mod ssh_proxy_boot;
mod ssh_proxy_class;
mod start_notice;
mod startup_override;
mod stop_budget;
#[cfg(unix)]
mod test_private_agent;
pub use seal::extra_write_paths;
mod test_bridge;

use bootstrap::{
    build_lock, load_muragent_and_home, read_flag_profile_from_args, resolve_embedded_agent_home,
};
pub use bootstrap::{resolve_model_entry, stale_cap_warnings};
#[cfg(test)]
use dispatch::HitlRespondHandler;
use dispatch::build_dispatcher;
pub use identity::{ValidationError, grace_cleanup_if_expired, validate_tcp_entitlement};
pub use test_bridge::{
    BridgeTestHandle, spawn_bridge_for_test_with_id, spawn_telegram_bridge_for_test,
};
#[cfg(unix)]
pub use test_private_agent::start_private_agent_for_test;

// Drives the real unix-socket transport, which does not exist on Windows.
#[cfg(all(test, unix))]
mod self_approval_tests;

#[cfg(test)]
mod hitl_tests;
#[cfg(test)]
mod startup_override_tests;

/// The runtime's `main`. Synchronous on purpose: everything up to the B1
/// seal runs on this one thread with no tokio runtime, and only then is the
/// runtime built. On Linux the seal confines the calling thread and threads
/// it creates later, never threads that already exist, so a runtime built
/// first left every worker — and every `bash` child spawned from one —
/// unconfined (GHSA-j8mv-27gx-x7xj). See `seal.rs`.
pub fn entrypoint() -> anyhow::Result<()> {
    // Handle --help/--version before any side effects (process-group changes,
    // socket creation, serve loop). Running the symlink with these flags must
    // print and exit, not silently start the supervisor daemon.
    let argv: Vec<String> = std::env::args().collect();
    // Unix only: the shim dials the agent's unix socket, which Windows has
    // no `tokio::net` equivalent of. Absent there rather than present and
    // broken — and gated as one block, since gating only the binding would
    // leave the branch below referring to a name that does not exist.
    #[cfg(unix)]
    if argv.get(1).map(String::as_str) == Some("mcp-shim") {
        let socket = crate::subcommand::flag_value(&argv, "--socket")
            .ok_or_else(|| anyhow::anyhow!("mcp-shim: --socket is required"))?;
        let task_id = crate::subcommand::flag_value(&argv, "--task-id")
            .ok_or_else(|| anyhow::anyhow!("mcp-shim: --task-id is required"))?;
        return boot::build_runtime()?.block_on(crate::mcp_shim::run(
            std::path::PathBuf::from(socket),
            task_id,
        ));
    }
    #[cfg(unix)]
    crate::sandbox::ssh_bypass::child::exit_if_probe(&argv);
    if boot::print_meta_flag(&argv) {
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    // 0. Become our own process-group leader so a parent (CLI launcher
    //    or future GUI sidecar manager) can SIGTERM the entire tree —
    //    runtime + every spawned MCP child — via `kill(-pgid, …)`. On
    //    failure (e.g. already a session leader) we log and continue;
    //    the runtime still runs but children may need explicit cleanup.
    //    See `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md` § 4.4.
    #[cfg(unix)]
    {
        // SAFETY: setpgid(0,0) makes the calling process its own pgid
        // leader; valid POSIX semantics, no preconditions beyond not
        // already being a session leader.
        let rc = unsafe { libc::setpgid(0, 0) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            warn!("setpgid(0,0) failed: {err} — process-group kill may not reach MCP children");
        }
    }

    // 1. Decide whether this binary carries an embedded agent.
    //    Embedded mode short-circuits MUR_HOME-based discovery and points
    //    agent_home at the per-binary cache extraction dir, unless the
    //    operator overrides via MUR_AGENT_EXTERNAL_PROFILE.
    let embedded_override = std::env::var_os("MUR_AGENT_EXTERNAL_PROFILE").is_some();
    let mur_home = mur_common::home::mur_home();
    let load_path = crate::subcommand::flag_value(&argv, "--load");
    let agent_home = if let Some(path) = load_path {
        match load_muragent_and_home(&path, &mur_home) {
            Ok(home) => home,
            Err(e) => {
                eprintln!("error[load]: {e}");
                std::process::exit(1);
            }
        }
    } else if crate::export::bin_embed::has_embedded_agent() && !embedded_override {
        match resolve_embedded_agent_home() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error[embedded_extract]: {e}");
                std::process::exit(1);
            }
        }
    } else {
        // Determine profile name from argv[0] (or --profile)
        let argv0 = std::env::args().next().unwrap_or_default();
        let name = match extract_profile_name(&argv0) {
            Ok(n) => n,
            Err(DispatchError::BareRuntime) => read_flag_profile_from_args()?,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        };
        let candidate = mur_home.join("agents").join(&name);
        // Verify_name_match runs after profile load below for both branches.
        // Stash the argv0-derived name so the post-load check can use it.
        unsafe {
            std::env::set_var("MUR_RUNTIME_EXPECTED_NAME", &name);
        }
        candidate
    };

    // ── B0 M10: install redacted crashlog hook now that agent_home
    // is resolved. Any panic from this point on (including tokio
    // tasks spawned later) writes to <agent_home>/crashlogs/<ts>.log
    // with the M8.1 redactor applied. The unredacted panic still
    // surfaces on stderr via the chained previous hook.
    crate::crashlog::install_panic_hook(agent_home.clone());

    bootstrap::migrate_bundled_spawn_grant(&agent_home, &mur_home); // #1984, pre-pin
    bootstrap::migrate_concierge_spawn_mode(&agent_home, &mur_home); // pre-pin
    let mut profile = match Profile::load(&agent_home) {
        Ok(p) => p,
        Err(e) => {
            pin::refuse(&agent_home, &format!("error[profile_invalid]: {e}"));
        }
    };
    // Non-interactive model rebind for headless/server use (§7.5). Honored
    // by build_provider_runner via resolve_model_entry, which prefers
    // model_ref over the inline model: block.
    if let Some(model_ref) = crate::subcommand::flag_value(&argv, "--model") {
        info!(model_ref = %model_ref, "overriding model binding from --model");
        profile.inner.model_ref = Some(model_ref);
    }
    if let Some(expected) = std::env::var_os("MUR_RUNTIME_EXPECTED_NAME") {
        let expected = expected.to_string_lossy().into_owned();
        if let Err(e) = verify_name_match(&expected, &profile.inner.name) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }

    // 2a. Acquire running.lock, then startup override steps 2–4 — both
    //     BEFORE pin::verify so a refused boot cannot leave the file behind,
    //     and lock first so a duplicate boot exits before it can consume the
    //     override (see `lock_then_consume`). A refused boot exits holding
    //     the flock; the kernel drops it with the process. `no_hitl` is
    //     applied below as the runner's waiver; the other flags at step 2c.
    let lock_path = agent_home.join("running.lock");
    let (lock_handle, pending_override) = startup_override::lock_then_consume(
        &lock_path,
        &mur_home,
        &agent_home,
        &profile.inner.name,
        profile.inner.lifecycle.stop_timeout_secs,
        chrono::Utc::now(),
    )
    .map_err(|e| anyhow::anyhow!("already running ({e})"))?;

    // 2b. #712: refuse entitlements that changed outside MUR (the agent's own
    //     bash can rewrite profile.yaml on Linux, where Landlock cannot deny it).
    let pin_trust = match pin::verify(&profile, &agent_home, &mur_home) {
        pin::Verdict::Start(t) => t,
        pin::Verdict::Refuse(msg) => pin::refuse(&agent_home, &msg),
    };
    // Past the refusal checks: a marker from an earlier refusal is stale, and
    // leaving it would stop the service manager respawning a later crash.
    mur_common::start_refusal::clear(&agent_home);
    // 2c. Startup override step 6: merge the boundary fields into the
    //     in-memory profile only — after the pin (which checks the file) and
    //     before the seal (which must be built from the merged value).
    let merged = startup_override::merge_into(&pending_override, &mut profile.inner);

    // 3. Warn on loose entitlements
    for w in detect_warnings(&profile.inner) {
        warn!(kind = ?w.kind, "{}", w.message);
    }

    // 3a. Load agent identity — see `identity::load_boot_identity`.
    let identity = Arc::new(identity::load_boot_identity(&agent_home));
    if let Err(msg) = identity::pin_identity_if_missing(&agent_home) {
        pin::refuse(&agent_home, &msg);
    }

    // 3b. M6.1: grace-period cleanup. If profile.identity.grace_expires_at
    //     has passed, shred identity.key.prev + clear the previous_pubkey
    //     fields in profile.yaml. Best-effort; log warnings on failure.
    if let Err(e) = grace_cleanup_if_expired(&agent_home, &profile.inner) {
        warn!(error = %e, "grace-period cleanup failed");
    }

    // 3b'. The one boot read of config.yaml and the global limits seed, both
    //      BEFORE the seal (#1796): a post-seal read can fail on an atomically
    //      replaced file and silently fall back to defaults.
    let boot_cfg = crate::supervisor_runner::load_boot_config(&mur_home);
    let boot_global_limits = match mur_common::limits::load_global(&mur_home) {
        Ok(g) => g.limits,
        Err(e) => {
            tracing::warn!(error = %e, "global limits did not load at boot; running on built-in defaults");
            Default::default()
        }
    };
    // 3c–3d. Pre-seal setup and the B1 seal (`seal.rs`); the runtime after.
    let pre = seal::prepare_and_seal(&mut profile, &agent_home, &mur_home, pin_trust)?;
    boot::build_runtime()?.block_on(serve::serve(boot::Boot {
        mur_home,
        agent_home,
        profile,
        identity,
        lock_path,
        lock_handle,
        pending_override,
        merged,
        boot_cfg,
        boot_global_limits,
        pre,
    }))
}
