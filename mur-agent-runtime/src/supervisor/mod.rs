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
use crate::transport::unix_socket::serve_unix_gated;
use crate::transport::webhook;
use crate::watch_scheduler::WatchScheduler;
use mur_common::identity::AgentIdentity;
use mur_common::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, LockFile, agent::LockTransports};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tracing::{info, warn};

mod bootstrap;
mod dispatch;
mod identity;
mod pin;
mod seal;
mod test_bridge;

use bootstrap::{
    load_muragent_and_home, parent_pid, read_flag_profile_from_args, resolve_embedded_agent_home,
};
pub use bootstrap::{resolve_model_entry, stale_cap_warnings};
#[cfg(test)]
use dispatch::HitlRespondHandler;
use dispatch::build_dispatcher;
pub use identity::{ValidationError, grace_cleanup_if_expired, validate_tcp_entitlement};
pub use test_bridge::{
    BridgeTestHandle, spawn_bridge_for_test_with_id, spawn_telegram_bridge_for_test,
};

// Drives the real unix-socket transport, which does not exist on Windows.
#[cfg(all(test, unix))]
mod self_approval_tests;

#[cfg(test)]
mod hitl_tests;

pub async fn entrypoint() -> anyhow::Result<()> {
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
        return crate::mcp_shim::run(std::path::PathBuf::from(socket), task_id).await;
    }
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
    if crate::subcommand::has_flag(&argv, &["--build-id"]) {
        println!("{}", mur_common::build::SHORT_SHA);
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

    // 2b. #712: refuse entitlements that changed outside MUR (the agent's own
    //     bash can rewrite profile.yaml on Linux, where Landlock cannot deny it).
    if let pin::Verdict::Refuse(msg) = pin::verify(&profile, &agent_home, &mur_home) {
        eprintln!("{msg}");
        std::process::exit(1);
    }

    // 3. Warn on loose entitlements
    for w in detect_warnings(&profile.inner) {
        warn!(kind = ?w.kind, "{}", w.message);
    }

    // 3a. Load agent identity (Ed25519 keypair) for Noise-XK TCP transport.
    //     If missing, fall back to an ephemeral identity and warn that
    //     cross-host TCP won't work (peers can't verify our static key).
    // #850 option (c) step 2: move THIS agent's private key out of the agents
    // tree, before it is loaded. Runs here rather than from `mur update`
    // because that is not the only upgrade path — `build.sh --install` +
    // `mur agent restart --stale` never invokes it. Scoped to one key, so
    // concurrent agent starts cannot contend.
    //
    // Advisory: a failure leaves the key where it is, and step 1's fallback
    // still loads it. What must NOT happen is starting anyway after a refusal
    // to overwrite a different key — that case is reported loudly and the key
    // is left untouched, so the next start retries.
    match mur_common::identity::migrate_private_key(&agent_home) {
        Ok(true) => info!("private key migrated out of the agents tree"),
        Ok(false) => {}
        Err(e) => warn!(
            error = %e,
            "could not migrate the private key; it stays where it is and still \
             loads, but this agent's key remains readable to siblings created \
             after the sandbox sealed"
        ),
    }

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

    // 3c–3d. Pre-seal setup and the B1 sandbox seal — see `seal.rs`.
    let seal::Sealed {
        egress_proxy,
        secrets,
        approval_token,
        sandbox_record,
    } = seal::prepare_and_seal(&profile, &agent_home, &mur_home).await?;
    // 4–4a. Telemetry writer, hook chain, skills — extracted to prepare_runtime.
    let socket_enabled = profile.inner.transport.socket.enabled
        && profile.inner.transport.socket.bind.starts_with("unix://");
    let (
        writer,
        stdio_notif_rx,
        sock_notif_rx,
        sock_notif_tx,
        hook_chain,
        hook_ctx,
        hook_cancel,
        runtime_skills,
        skills_cfg,
        memory_cfg,
    ) = crate::supervisor_runner::prepare_runtime(&agent_home, &profile, socket_enabled).await?;

    // 5. Acquire running.lock
    let lock_path = agent_home.join("running.lock");
    let lock_handle =
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
    let pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let hitl_timeout_secs = profile.inner.hitl.timeout_secs;
    for line in stale_cap_warnings(&profile.inner.hitl) {
        tracing::warn!(agent = %profile.inner.name, "{line}");
        eprintln!("warning: {line}");
    }
    // The two scopes this process can see (spec §3.4). The fleet's clock, when
    // there is one, arrives per turn in the A2A `limits` parameter.
    let limits = (
        mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).limits,
        profile.inner.limits.clone(),
    );
    let (runner, llm_for_companion, mcp_pool, model_switch) =
        crate::supervisor_runner::build_provider_runner(
            force_echo,
            &agent_home,
            &profile,
            egress_proxy,
            runtime_skills.clone(),
            skills_cfg.clone(),
            memory_cfg.clone(),
            &hook_chain,
            &hook_ctx,
            &hook_cancel,
            Some(pending_approvals.clone()),
            Some(sock_notif_tx.clone()),
            hitl_timeout_secs,
            limits,
            Some(writer.sender()),
            identity.clone(),
            secrets.clone(),
            // `Some` on every path that reaches here; `None` would mean no
            // seal was attempted, which must not count as sealed.
            sandbox_record.as_ref().is_some_and(|r| r.enforcing),
        )
        .await?;
    let dispatcher = Arc::new(build_dispatcher(
        &profile_arc,
        &runner,
        &mur_home,
        sock_notif_tx.clone(),
        pending_approvals,
        &identity,
        &profile.inner.name,
        profile.inner.identity.key_version,
        model_switch,
        runtime_skills.clone(),
        secrets.clone(),
        crate::hitl::authority::ApprovalAuthority::new(
            approval_token,
            sandbox_record.as_ref().is_some_and(|r| r.enforcing),
        ),
    ));

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
        let policy = std::sync::Arc::new(crate::communication_policy::AcceptPolicy {
            accepts_from: profile.inner.communication.accepts_from.clone(),
            agents_dir: mur_home.join("agents"),
            self_pid: std::process::id(),
        });
        transport_tasks.push(tokio::spawn(async move {
            let _ = serve_unix_gated(d, bind, sock_notif_rx, Some(policy)).await;
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
                // TCP is request/reply with no per-connection streaming sink.
                let resp = match d
                    .dispatch(req, &crate::protocol::a2a_server::RequestContext::none())
                    .await
                {
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
        build_sha: mur_common::build::SHORT_SHA.to_string(),
        proto_version: mur_common::build::A2A_PROTO_VERSION,
        sandbox: sandbox_record,
    };
    write_lock(&lock_path, &lock)?;
    info!("agent {} ({}) ready", profile.inner.name, profile.inner.id);

    // 8c. C4 — cron scheduler. Spawn one loop per lifecycle.schedule entry.
    //     Each loop selects on a shared CancellationToken so SIGTERM (which
    //     calls t.abort() on all transport_tasks) also stops in-flight entries.
    if !profile.inner.lifecycle.schedule.is_empty() {
        // #1125: give the scheduler somewhere to leave each fired turn's
        // reply. Without a sink the turn runs and its output is discarded.
        let cs = CronScheduler::new(profile.inner.lifecycle.schedule.clone(), runner.clone())
            .with_sink(crate::scheduler::ScheduleSink {
                mur_home: mur_home.clone(),
                agent: profile.inner.name.clone(),
                identity: identity.clone(),
                key_version: profile.inner.identity.key_version,
                locale: profile.inner.companion.locale.clone(),
            });
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

    // 8d-bis. Proactive co-watching scheduler (spec §6). Cheap when no session is
    //         active (one file read per tick); only acts while watch.json is active.
    {
        let ws = WatchScheduler::new(
            runner.clone(),
            mur_home.to_path_buf(),
            profile.inner.companion.proactive.quiet_hours.clone(),
        );
        transport_tasks.push(ws.spawn());
        info!("WatchScheduler started");
    }

    // 8e. E3 — agent-side sleep cycle: flush evidence outbox + pull snapshot.
    {
        let name = profile.inner.name.clone();
        transport_tasks.push(crate::federation::spawn_agent_sleep_cycle(
            name,
            identity.clone(),
        ));
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
    // Fire observe-hooks before transport teardown so the telemetry
    // event makes it into the JSONL file.
    hook_chain
        .on_shutdown(&hook_ctx, ShutdownReason::Sigterm, &hook_cancel)
        .await;
    // Stop accepting new turns, then cooperatively wait for any in-flight turn
    // to finish before tearing down transports. Never SIGKILL mid-flight work.
    runner.begin_drain();
    let drain_timeout = std::time::Duration::from_secs(profile.inner.lifecycle.stop_timeout_secs);
    if runner.await_idle(drain_timeout).await {
        info!("task runner drained cleanly");
    } else {
        warn!(
            "task runner did not drain within {}s; tearing down anyway",
            profile.inner.lifecycle.stop_timeout_secs
        );
    }
    // Every bash job is a process group this runtime started; nothing else
    // will end them once we are gone (spec D3/D9).
    let killed = runner.kill_all_jobs().await;
    if killed > 0 {
        info!(jobs = killed, "ended running bash jobs");
    }
    for t in transport_tasks {
        t.abort();
    }
    if let Some(pool) = mcp_pool {
        pool.shutdown().await;
    }
    writer
        .emit(Event::Warning {
            kind: "shutdown".into(),
            message: "SIGTERM".into(),
        })
        .await;
    writer.flush().await;
    lock_handle.release();
    Ok(())
}
