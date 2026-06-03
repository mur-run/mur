//! Agent runtime entrypoint — assembles profile, dispatcher, telemetry, and
//! drives the stdio (and optionally Unix-socket) transports until SIGTERM.

use anyhow::Context;

use crate::entitlements::detect_warnings;
use crate::hooks::ShutdownReason;
use crate::idle_scheduler::IdleScheduler;
use crate::lock_file::{LockHandle, write_lock};
use crate::multi_call::{DispatchError, extract_profile_name, verify_name_match};
use crate::profile::Profile;
use crate::protocol::a2a_server::Dispatcher;
use crate::protocol::methods::{
    card::CardHandler,
    message_send::MessageSendHandler,
    tasks::{TasksCancelHandler, TasksGetHandler, TasksListHandler},
};
use crate::scheduler::CronScheduler;
#[cfg(unix)]
use crate::socket_path::resolve_bind_target;
use crate::task_runner::TaskRunner;
use crate::telemetry_writer::{Event, TelemetryWriter};
use crate::transport::stdio::serve_stdio;
use crate::transport::tcp::{TcpTransportConfig, spawn_tcp_listener};
#[cfg(unix)]
use crate::transport::unix_socket::serve_unix;
use crate::transport::webhook;
use mur_common::identity::AgentIdentity;
use mur_common::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, LockFile, agent::LockTransports};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

pub async fn entrypoint() -> anyhow::Result<()> {
    // Handle --help/--version before any side effects (process-group changes,
    // socket creation, serve loop). Running the symlink with these flags must
    // print and exit, not silently start the supervisor daemon.
    let argv: Vec<String> = std::env::args().collect();
    if crate::subcommand::has_flag(&argv, &["--help", "-h"]) {
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
        return Ok(());
    }
    if crate::subcommand::has_flag(&argv, &["--version", "-V"]) {
        println!("mur-agent-runtime {}", env!("CARGO_PKG_VERSION"));
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
    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"));
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

    let mut profile = match Profile::load(&agent_home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error[profile_invalid]: {e}");
            std::process::exit(1);
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

    // 3. Warn on loose entitlements
    for w in detect_warnings(&profile.inner) {
        warn!(kind = ?w.kind, "{}", w.message);
    }

    // 3a. Load agent identity (Ed25519 keypair) for Noise-XK TCP transport.
    //     If missing, fall back to an ephemeral identity and warn that
    //     cross-host TCP won't work (peers can't verify our static key).
    let identity = Arc::new(match AgentIdentity::load(&agent_home) {
        Ok(id) => id,
        Err(e) => {
            warn!(
                error = %e,
                "no identity keypair found; generating ephemeral (cross-host TCP disabled)"
            );
            AgentIdentity::generate()
        }
    });

    // 3b. M6.1: grace-period cleanup. If profile.identity.grace_expires_at
    //     has passed, shred identity.key.prev + clear the previous_pubkey
    //     fields in profile.yaml. Best-effort; log warnings on failure.
    if let Err(e) = grace_cleanup_if_expired(&agent_home, &profile.inner) {
        warn!(error = %e, "grace-period cleanup failed");
    }

    // B1: apply OS-level kernel sandbox based on profile entitlements.
    // Called AFTER profile load (needs entitlements) and AFTER grace cleanup.
    // BEFORE telemetry writer (its file I/O must be within sandbox bounds).
    // BEFORE on_startup hooks.
    // On platforms without Landlock/SBPL support, returns enforcing=false — B0 still applies.
    match crate::sandbox::apply(&profile.inner.entitlements, &agent_home) {
        Ok(status) => {
            tracing::info!(
                platform = %status.platform,
                effective_abi = ?status.effective_abi,
                enforcing = status.enforcing,
                "B1 sandbox applied"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "B1 sandbox::apply failed; running advisory-only (B0 remains active)");
        }
    }

    // 4–4a. Telemetry writer, hook chain, skills — extracted to prepare_runtime.
    let socket_enabled = profile.inner.transport.socket.enabled
        && profile.inner.transport.socket.bind.starts_with("unix://");
    let (
        writer,
        stdio_notif_rx,
        sock_notif_rx,
        hook_chain,
        hook_ctx,
        hook_cancel,
        runtime_skills,
        skills_cfg,
    ) = crate::supervisor_runner::prepare_runtime(&agent_home, &profile, socket_enabled).await?;

    // 5. Acquire running.lock
    let lock_path = agent_home.join("running.lock");
    let _lock_handle =
        LockHandle::acquire(&lock_path).map_err(|e| anyhow::anyhow!("already running ({e})"))?;

    // 6. Build dispatcher (shared Arc so multiple transports can read it)
    let profile_arc = Arc::new(profile.clone());
    // E4: select runner backend based on profile.model.provider.
    // Ollama: real client. Other providers (anthropic/openai stubs not yet
    // implemented) fall through to echo. Setting MUR_AGENT_FORCE_ECHO=1
    // forces echo regardless of profile (useful for tests).
    let force_echo = std::env::var_os("MUR_AGENT_FORCE_ECHO").is_some();
    // Track C1 (M-c1.0.2): refuse to construct an LLM client when the profile
    // declares `entitlements.llm.mode = off` — i.e. the agent is a bridge.
    crate::llm::build_client(&profile.inner)
        .map_err(|e| anyhow::anyhow!("supervisor refusing LLM construction: {e}"))?;
    // `llm_for_companion` carries the real LLM client (None when echo/stub) so
    // the companion subsystem can share the same provider without a second dial.
    let (runner, llm_for_companion) = crate::supervisor_runner::build_provider_runner(
        force_echo,
        &profile,
        runtime_skills.clone(),
        skills_cfg.clone(),
        &hook_chain,
        &hook_ctx,
        &hook_cancel,
    )
    .await?;
    let dispatcher = Arc::new(build_dispatcher(&profile_arc, &runner, &mur_home));

    // 7. Transports
    let mut transport_tasks = vec![];
    let mut lock_transports = LockTransports {
        stdio: profile.inner.transport.stdio,
        unix_socket: None,
        tcp: None,
        webhook: None,
    };

    #[cfg(unix)]
    if socket_enabled {
        let canonical = PathBuf::from(
            profile
                .inner
                .transport
                .socket
                .bind
                .trim_start_matches("unix://"),
        );
        let res = resolve_bind_target(&canonical, &profile.inner.id)?;
        lock_transports.unix_socket = Some(canonical.to_string_lossy().to_string());
        let d = dispatcher.clone();
        let bind = res.bind_path.clone();
        transport_tasks.push(tokio::spawn(async move {
            let _ = serve_unix(d, bind, sock_notif_rx).await;
        }));
    }
    #[cfg(not(unix))]
    {
        let _ = sock_notif_rx;
    }

    // 7b. Conditionally spawn Noise-XK TCP listener (P0a.5).
    //     Must happen BEFORE write_lock so lock_transports.tcp carries the
    //     resolved local_addr (handles `:0` ephemeral binds).
    if profile.inner.transport.tcp.enabled && !profile.inner.transport.tcp.bind.is_empty() {
        // Entitlement gate (B8): ensure bind port is declared in entitlements
        if let Err(e) = validate_tcp_entitlement(&profile.inner) {
            anyhow::bail!("TCP transport misconfigured: {e}");
        }
        let d = dispatcher.clone();
        let handler = Arc::new(move |payload: Vec<u8>| {
            let d = d.clone();
            async move {
                let req: JsonRpcRequest = match serde_json::from_slice(&payload) {
                    Ok(r) => r,
                    Err(e) => {
                        let err_resp = JsonRpcResponse {
                            jsonrpc: "2.0".into(),
                            id: serde_json::Value::Null,
                            result: None,
                            error: Some(JsonRpcError {
                                code: -32700,
                                message: format!("parse error: {e}"),
                                data: None,
                            }),
                        };
                        return Ok::<_, std::io::Error>(
                            serde_json::to_vec(&err_resp)
                                .unwrap_or_else(|_| br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}"#.to_vec()),
                        );
                    }
                };
                // Dispatcher::dispatch returns Result<JsonRpcResponse, HandlerError>;
                // map HandlerError to a JSON-RPC error envelope so the caller
                // always receives a well-formed response frame.
                let resp = match d.dispatch(req).await {
                    Ok(r) => r,
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".into(),
                        id: serde_json::Value::Null,
                        result: None,
                        error: Some(JsonRpcError {
                            code: e.code(),
                            message: e.to_string(),
                            data: None,
                        }),
                    },
                };
                serde_json::to_vec(&resp).map_err(|e| std::io::Error::other(e.to_string()))
            }
        });
        let (tcp_shutdown_tx, tcp_shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
        // Build the inbound peer allowlist: each trusted_peer's Ed25519 identity
        // mapped to its X25519 static key (what Noise-XK authenticates). Invalid
        // pubkeys are skipped with a warning. Fail-closed: if this ends up empty,
        // every inbound TCP connection is rejected by the listener.
        let allowed_peers: Vec<[u8; 32]> = profile
            .inner
            .trusted_peers
            .iter()
            .filter_map(|p| {
                match mur_common::identity::x25519_pub_from_multibase(&p.pubkey_multibase) {
                    Ok(k) => Some(k),
                    Err(e) => {
                        warn!(peer = %p.name, error = %e, "skipping trusted_peer with invalid pubkey");
                        None
                    }
                }
            })
            .collect();
        if allowed_peers.is_empty() {
            warn!(
                "TCP transport enabled but no valid trusted_peers — all inbound \
                 TCP connections will be rejected (fail-closed)"
            );
        }
        let allowed_peers = Arc::new(allowed_peers);
        let tcp_handle = spawn_tcp_listener(
            TcpTransportConfig {
                bind: profile.inner.transport.tcp.bind.clone(),
            },
            identity.clone(),
            handler,
            allowed_peers,
            tcp_shutdown_rx,
        )
        .await?;
        let tcp_addr = tcp_handle.local_addr();
        info!("TCP Noise listener at {tcp_addr}");
        lock_transports.tcp = Some(tcp_addr.to_string());
        // Keep the shutdown sender alive inside the wrapper task; the task is
        // aborted during graceful shutdown, which cancels the listener.
        transport_tasks.push(tokio::spawn(async move {
            let _keep_tx_alive = tcp_shutdown_tx;
            tcp_handle.await_shutdown().await;
        }));
    }

    // 7c. Conditionally spawn C5 webhook listener.
    //     Mirrors 7b's pattern: bind synchronously so failures
    //     surface before the agent claims ready, then drive
    //     `axum::serve` on a transport task. HMAC secret comes
    //     from the OS keychain — the `hmac_secret_ref` in
    //     `profile.yaml` is the `service:account` lookup key
    //     written by `mur agent webhook secret-set`.
    if profile.inner.transport.webhook.enabled && !profile.inner.transport.webhook.bind.is_empty() {
        let cfg = &profile.inner.transport.webhook;
        let (svc, acct) = cfg.hmac_secret_ref.split_once(':').ok_or_else(|| {
            anyhow::anyhow!(
                "transport.webhook.hmac_secret_ref must be `service:account`, got `{}`",
                cfg.hmac_secret_ref,
            )
        })?;
        let secret = mur_common::secret::keychain_get(svc, acct)
            .await
            .with_context(|| format!("read webhook HMAC secret {svc}:{acct}"))?
            .ok_or_else(|| anyhow::anyhow!(
                "webhook enabled but HMAC secret missing — run `mur agent webhook secret-set {}`",
                profile.inner.name,
            ))?;
        use secrecy::ExposeSecret;
        let state = webhook::WebhookState::new(
            &profile.inner.name,
            secret.expose_secret().as_bytes(),
            agent_home.clone(),
        );
        let handle = webhook::spawn_webhook_listener(&cfg.bind, cfg.port, state).await?;
        info!(
            "webhook listener at http://{} (slug: {})",
            handle.local_addr, profile.inner.name,
        );
        // Stash the local addr in the lock file so peers (and the
        // commander) can discover the live URL without re-reading
        // profile.yaml; mirrors the TCP listener's lock entry.
        lock_transports.webhook = Some(format!("http://{}", handle.local_addr));
        transport_tasks.push(tokio::spawn(async move {
            handle.await_shutdown().await;
        }));
    }

    // 8. Write running.lock
    let lock = LockFile {
        schema: 1,
        uuid: profile.inner.id.clone(),
        name: profile.inner.name.clone(),
        pid: std::process::id(),
        ppid: parent_pid(),
        started_at: chrono::Utc::now().to_rfc3339(),
        binary_version: format!("mur-agent-runtime {}", env!("CARGO_PKG_VERSION")),
        transports: lock_transports,
        card_digest: profile.digest.clone(),
        capabilities: profile.inner.capabilities.clone(),
    };
    write_lock(&lock_path, &lock)?;
    info!("agent {} ({}) ready", profile.inner.name, profile.inner.id);

    // 8c. C4 — cron scheduler. Spawn one loop per lifecycle.schedule entry.
    //     Each loop selects on a shared CancellationToken so SIGTERM (which
    //     calls t.abort() on all transport_tasks) also stops in-flight entries.
    if !profile.inner.lifecycle.schedule.is_empty() {
        let cs = CronScheduler::new(profile.inner.lifecycle.schedule.clone(), runner.clone());
        transport_tasks.push(cs.spawn());
        info!(
            count = profile.inner.lifecycle.schedule.len(),
            "CronScheduler started"
        );
    }

    // 8d. C6 — idle scheduler. Wakes every 30 s, fires triggers when the
    //     agent has been idle for >= IdleTrigger.after_secs.
    if !profile.inner.lifecycle.idle_triggers.is_empty() {
        let is = IdleScheduler::new(
            profile.inner.lifecycle.idle_triggers.clone(),
            runner.clone(),
            profile.inner.companion.proactive.quiet_hours.clone(),
        );
        transport_tasks.push(is.spawn());
        info!(
            count = profile.inner.lifecycle.idle_triggers.len(),
            "IdleScheduler started"
        );
    }

    // 8e. E3 — agent-side sleep cycle: flush evidence outbox + pull snapshot.
    {
        let name = profile.inner.name.clone();
        transport_tasks.push(crate::federation::spawn_agent_sleep_cycle(name));
        info!("agent sleep-cycle spawned");
    }

    // 8.5 — bridge agents (LLM disabled by entitlement) emit a 30 s
    //       heartbeat so peers can classify them via
    //       `bridge::beacon::bridge_status_for_peer` (running.lock mtime
    //       refreshes whenever the writer task appends a JSONL line).
    if profile.inner.entitlements.llm.mode == mur_common::LlmMode::Off {
        let beacon =
            crate::bridge::beacon::BridgeBeacon::new(profile.inner.name.clone(), writer.sender());
        transport_tasks.push(beacon.spawn());
        info!(name = %profile.inner.name, "spawned BridgeBeacon (30 s heartbeat)");
    }

    // 8b. Companion subsystem (Phase 1.1 M5.7).
    //     Returns None when profile.companion.enabled is false — zero-cost path.
    let companion_clock =
        Arc::new(crate::companion::clock::SystemClock) as Arc<dyn crate::companion::clock::Clock>;
    if let Some(llm) = llm_for_companion {
        let companion = match crate::companion::Companion::new(
            &profile.inner,
            &agent_home,
            companion_clock,
            llm,
        ) {
            Ok(Some(c)) => Some(c),
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, "companion init failed; continuing without companion");
                None
            }
        };
        if let Some(c) = companion {
            let handle = c.clone_handle();
            transport_tasks.push(tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
                // First tick fires immediately — we want to wait 60s before the first.
                interval.tick().await;
                loop {
                    interval.tick().await;
                    handle.run_tick().await;
                }
            }));
        }
    } else if profile.inner.companion.enabled {
        warn!("companion is enabled but no LLM provider is configured; companion disabled");
    }

    // 9. Install signal handlers BEFORE spawning transports so SIGTERM
    //    cannot kill the process before the handler is ready (race between
    //    the multi-threaded tokio runtime's worker threads and this task).
    #[cfg(unix)]
    let (mut sigterm, mut sigint) = {
        use tokio::signal::unix::{SignalKind, signal};
        let sigterm = signal(SignalKind::terminate())?;
        let sigint = signal(SignalKind::interrupt())?;
        (sigterm, sigint)
    };

    // 10. Drive stdio in the foreground so SIGTERM can unblock the process
    //    even while stdin is idle.
    if profile.inner.transport.stdio {
        let d = dispatcher.clone();
        transport_tasks.push(tokio::spawn(async move {
            let _ = serve_stdio(d, tokio::io::stdin(), tokio::io::stdout(), stdio_notif_rx).await;
        }));
    }

    // 11. Wait for SIGTERM / SIGINT (or Ctrl-C on Windows)
    #[cfg(unix)]
    {
        tokio::select! {
            _ = sigterm.recv() => info!("SIGTERM received"),
            _ = sigint.recv() => info!("SIGINT received"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("Ctrl-C received");
    }

    // 11. Graceful shutdown
    info!("begin graceful shutdown");
    let _deadline = std::time::Duration::from_secs(profile.inner.lifecycle.stop_timeout_secs);
    // Fire observe-hooks before transport teardown so the telemetry
    // event makes it into the JSONL file.
    hook_chain
        .on_shutdown(&hook_ctx, ShutdownReason::Sigterm, &hook_cancel)
        .await;
    // TaskRunner active-task cancellation is future work (P0b); for now
    // we just tear down transports and drain telemetry.
    for t in transport_tasks {
        t.abort();
    }
    writer
        .emit(Event::Warning {
            kind: "shutdown".into(),
            message: "SIGTERM".into(),
        })
        .await;
    writer.flush().await;
    let _ = std::fs::remove_file(&lock_path);
    Ok(())
}

fn build_dispatcher(
    profile: &Arc<Profile>,
    runner: &Arc<TaskRunner>,
    mur_home: &Path,
) -> Dispatcher {
    let mut d = Dispatcher::new();
    d.register("agent/card", Box::new(CardHandler::new(profile.clone())));
    d.register(
        "message/send",
        Box::new(MessageSendHandler::new(runner.clone())),
    );
    d.register("tasks/get", Box::new(TasksGetHandler::new(runner.clone())));
    d.register(
        "tasks/cancel",
        Box::new(TasksCancelHandler::new(runner.clone())),
    );
    d.register(
        "tasks/list",
        Box::new(TasksListHandler::new(runner.clone())),
    );
    d.register(
        "skills/get",
        Box::new(crate::protocol::methods::skills::SkillsGetHandler::new(
            mur_home.to_path_buf(),
        )),
    );
    d
}

fn parent_pid() -> u32 {
    #[cfg(unix)]
    unsafe {
        libc::getppid() as u32
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Test-only helper: spawn just the bridge-side of the supervisor for a
/// telegram bridge agent rooted at `agent_dir`.
///
/// Drives the same code path the real supervisor takes when
/// `entitlements.llm.mode = off` — instantiates a [`TelemetryWriter`]
/// pointed at `agent_dir/telemetry/`, spawns a [`crate::bridge::beacon::BridgeBeacon`]
/// keyed on `bridge_id` (defaults to `"tg"`), and writes a fresh
/// `running.lock` so peers' `bridge_status_for_peer` classifies the
/// agent as `Running` immediately.
///
/// Used by M-c2.6.3 and downstream `mur agent doctor` integration tests
/// to verify the heartbeat path is wired without spinning up a full
/// runtime (no telegram bot token, no MCP child, no transport listener).
///
/// Returns a [`BridgeTestHandle`]; call `.shutdown().await` to abort
/// the beacon task and remove `running.lock`.
pub async fn spawn_telegram_bridge_for_test(
    agent_dir: &std::path::Path,
) -> std::io::Result<BridgeTestHandle> {
    spawn_bridge_for_test_with_id(agent_dir, "tg").await
}

/// Lower-level form of [`spawn_telegram_bridge_for_test`] that lets
/// callers pick a custom `bridge_id` (the value embedded in
/// `telemetry/bridge_alive` payloads).
pub async fn spawn_bridge_for_test_with_id(
    agent_dir: &std::path::Path,
    bridge_id: &str,
) -> std::io::Result<BridgeTestHandle> {
    std::fs::create_dir_all(agent_dir)?;
    let lock_path = agent_dir.join("running.lock");
    std::fs::write(&lock_path, b"{}")?;

    let telemetry_dir = agent_dir.join("telemetry");
    let (writer, _notif_rx) = TelemetryWriter::new(
        telemetry_dir,
        bridge_id.to_string(),
        format!("test-{bridge_id}"),
    )
    .await?;

    let beacon = crate::bridge::beacon::BridgeBeacon::new(bridge_id.to_string(), writer.sender());
    let join = beacon.spawn();

    Ok(BridgeTestHandle {
        beacon: Some(join),
        lock_path,
    })
}

/// Handle returned by [`spawn_telegram_bridge_for_test`]. Aborts the
/// background heartbeat task and tidies `running.lock` on
/// [`BridgeTestHandle::shutdown`] (or on drop).
pub struct BridgeTestHandle {
    beacon: Option<tokio::task::JoinHandle<()>>,
    lock_path: PathBuf,
}

impl BridgeTestHandle {
    /// Abort the heartbeat task and remove `running.lock`. Idempotent.
    pub async fn shutdown(mut self) {
        if let Some(j) = self.beacon.take() {
            j.abort();
        }
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

impl Drop for BridgeTestHandle {
    fn drop(&mut self) {
        if let Some(j) = self.beacon.take() {
            j.abort();
        }
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Extract the binary's embedded agent (idempotent across runs) and
/// return the resolved agent_home directory.
fn resolve_embedded_agent_home() -> anyhow::Result<PathBuf> {
    #[cfg(feature = "embedded-agent")]
    {
        use crate::export::bin_embed::EMBEDDED_TAR;
        use crate::export::extract::{default_cache_base, extract_embedded_to};
        let info = extract_embedded_to(EMBEDDED_TAR, &default_cache_base())?;
        Ok(info.agent_home)
    }
    #[cfg(not(feature = "embedded-agent"))]
    {
        anyhow::bail!("embedded-agent feature not compiled in")
    }
}

/// `--load` path: install a `.muragent` into `mur_home/agents/<slug>` (reusing
/// the shared installer — validation, trust, extraction) and return that home.
/// Stashes the slug as the expected name so the post-load name check passes.
fn load_muragent_and_home(path: &str, mur_home: &Path) -> anyhow::Result<PathBuf> {
    use mur_common::muragent::installer;
    use mur_common::muragent::reader::MuragentArchive;

    let archive = MuragentArchive::read(Path::new(path))
        .map_err(|e| anyhow::anyhow!("read .muragent at {path}: {e}"))?;
    let outcome = installer::install(&archive, mur_home, "cli")
        .map_err(|e| anyhow::anyhow!("install .muragent: {e}"))?;
    let slug = outcome.manifest.agent.slug.clone();
    // SAFETY: single-threaded startup, before any tokio tasks spawn.
    unsafe {
        std::env::set_var("MUR_RUNTIME_EXPECTED_NAME", &slug);
    }
    Ok(mur_home.join("agents").join(&slug))
}

fn read_flag_profile_from_args() -> anyhow::Result<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--profile"
            && let Some(name) = args.next()
        {
            return Ok(name);
        }
        if let Some(n) = a.strip_prefix("--profile=") {
            return Ok(n.to_string());
        }
    }
    anyhow::bail!("bare mur-agent-runtime requires --profile <name>")
}

/// Error surfaced when the profile's declared TCP transport bind port is
/// inconsistent with entitlements (P0a.5).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(pub String);

/// Cross-check profile: if TCP is enabled, its bind port must be in
/// entitlements.network.inbound.ports (empty list = "no inbound").
pub fn validate_tcp_entitlement(
    p: &mur_common::agent::AgentProfile,
) -> Result<(), ValidationError> {
    if !p.transport.tcp.enabled {
        return Ok(());
    }
    let port = p
        .transport
        .tcp
        .bind
        .rsplit(':')
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| ValidationError("transport.tcp.bind missing parseable port".into()))?;
    if !p.entitlements.network.inbound.ports.contains(&port) {
        return Err(ValidationError(format!(
            "transport.tcp bound to :{port} but entitlements.network.inbound.ports does not allow it"
        )));
    }
    Ok(())
}

/// M6.1: clean up the previous identity material if the grace period has
/// passed. On Unix we attempt a best-effort `shred -u` on the private
/// `identity.key.prev` first; if `shred` is not available we fall back to
/// overwriting + removing.
pub fn grace_cleanup_if_expired(
    agent_home: &std::path::Path,
    profile: &mur_common::agent::AgentProfile,
) -> anyhow::Result<()> {
    let Some(expires_str) = profile.identity.grace_expires_at.as_deref() else {
        return Ok(());
    };
    let expires = match chrono::DateTime::parse_from_rfc3339(expires_str) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    if expires > chrono::Utc::now() {
        return Ok(()); // still in grace
    }

    let key_prev = agent_home.join("identity.key.prev");
    let pub_prev = agent_home.join("identity.pub.prev");

    if key_prev.exists() {
        shred_file(&key_prev)?;
    }
    if pub_prev.exists() {
        let _ = std::fs::remove_file(&pub_prev);
    }

    // Rewrite profile.yaml without the previous_pubkey fields.
    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)?;
    let mut p: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml)?;
    p.identity.previous_pubkey = None;
    p.identity.previous_key_version = None;
    p.identity.grace_expires_at = None;
    p.updated_at = chrono::Utc::now().to_rfc3339();
    let new_yaml = serde_yaml_ng::to_string(&p)?;
    let tmp = profile_path.with_extension("tmp");
    std::fs::write(&tmp, new_yaml.as_bytes())?;
    std::fs::rename(&tmp, &profile_path)?;

    tracing::info!("grace-period cleanup: shredded identity.key.prev + cleared previous_pubkey");
    Ok(())
}

#[cfg(unix)]
fn shred_file(path: &std::path::Path) -> anyhow::Result<()> {
    // Try `shred -u` first (GNU coreutils — available on most Linux + macOS
    // via `gshred` if installed). Fall back to overwrite + remove.
    let shred = std::process::Command::new("shred")
        .arg("-u")
        .arg(path)
        .status();
    if let Ok(s) = shred
        && s.success()
    {
        return Ok(());
    }
    // Fallback: overwrite with random bytes, then unlink.
    use std::io::Write as _;
    if let Ok(meta) = std::fs::metadata(path) {
        let len = meta.len() as usize;
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
            // 32-byte private key — simple zero-fill is sufficient.
            let zeros = vec![0u8; len.max(32)];
            let _ = f.write_all(&zeros);
            let _ = f.sync_all();
        }
    }
    let _ = std::fs::remove_file(path);
    Ok(())
}

#[cfg(not(unix))]
fn shred_file(path: &std::path::Path) -> anyhow::Result<()> {
    // Windows: best-effort overwrite + delete.
    use std::io::Write as _;
    if let Ok(meta) = std::fs::metadata(path) {
        let len = meta.len() as usize;
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
            let zeros = vec![0u8; len.max(32)];
            let _ = f.write_all(&zeros);
            let _ = f.sync_all();
        }
    }
    let _ = std::fs::remove_file(path);
    Ok(())
}

/// Resolve the effective `ModelEntry` for an agent. Prefers
/// `profile.model_ref` (looks it up in `~/.mur/models.yaml`); falls back to
/// the inline `model:` block when the field is unset.
pub fn resolve_model_entry(
    profile: &mur_common::agent::AgentProfile,
) -> anyhow::Result<mur_common::model::ModelEntry> {
    use anyhow::Context;
    use mur_common::model::{ModelEntry, ModelRegistry};
    if let Some(name) = profile.model_ref.as_deref() {
        let path = ModelRegistry::default_path()?;
        let reg = ModelRegistry::load_from(&path)
            .with_context(|| format!("load registry {}", path.display()))?;
        let entry = reg
            .models
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("model_ref {name:?} not found in {}", path.display()))?;
        Ok(entry.clone())
    } else {
        Ok(ModelEntry {
            provider: profile.model.provider.clone(),
            model: profile.model.name.clone(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::to_value(&profile.model.params).unwrap_or(serde_json::Value::Null),
            tier: None,
            cost_per_1k_tokens: None,
        })
    }
}
