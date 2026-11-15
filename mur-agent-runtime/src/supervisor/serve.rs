//! The post-seal half of startup: everything that runs inside the tokio
//! runtime once `seal::prepare_and_seal` has returned.

use super::*;
use crate::hooks::ShutdownReason;
use crate::idle_scheduler::IdleScheduler;
use crate::lock_file::write_lock;
use crate::scheduler::CronScheduler;
#[cfg(unix)]
use crate::socket_path::resolve_bind_target;
use crate::telemetry_writer::Event;
use crate::transport::stdio::serve_stdio;
use crate::transport::tcp::{TcpTransportConfig, spawn_tcp_listener};
#[cfg(unix)]
use crate::transport::unix_socket::serve_unix_gated;
use crate::transport::webhook;
use crate::watch_scheduler::WatchScheduler;
use anyhow::Context;
use mur_common::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, agent::LockTransports};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tracing::{info, warn};

/// Everything after the seal, inside the runtime.
pub(super) async fn serve(boot: boot::Boot) -> anyhow::Result<()> {
    let boot::Boot {
        mur_home,
        agent_home,
        mut profile,
        identity,
        lock_path,
        lock_handle,
        pending_override,
        merged,
        boot_cfg,
        boot_global_limits,
        pre,
    } = boot;
    let seal::Sealed {
        egress_proxy,
        secrets,
        approval_token,
        sandbox_record,
        ssh_child,
        ssh_gate,
    } = seal::start_after_seal(pre, &profile, &agent_home, &mur_home, &identity).await?;
    // `Some` on every path that reaches here; `None` would mean no seal was
    // attempted, which must not count as sealed. Every consumer below reads
    // this one value, so they cannot disagree about the boot.
    let enforcing = sandbox_record.as_ref().is_some_and(|r| r.enforcing);
    // Override step 8: an unsealed boot drops the relaxation it reports rejected.
    let (on_disk, override_changes) =
        startup_override::settle(merged, enforcing, &mut profile.inner);
    // 4. Hot-reloaded settings (profile.yaml hot fields + global limits),
    //    read per turn. Built after the seal: both stay readable. The merged
    //    profile is the base reloads layer onto (spec step 7); cold drift is
    //    measured against the file.
    let live = crate::live_profile::LiveSettings::with_on_disk(
        &agent_home,
        &mur_home,
        profile.inner.clone(),
        on_disk,
        boot_global_limits.clone(),
    );
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
    ) = crate::supervisor_runner::prepare_runtime(
        &agent_home,
        &profile,
        socket_enabled,
        &live.profile,
        &boot_cfg,
    )
    .await?;

    // 5. running.lock is held since step 2a.

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
    for line in stale_cap_warnings(&profile.inner.hitl).into_iter().chain(
        mur_common::hitl::floor_waiver::waiver_warnings(&profile.inner.entitlements.tools),
    ) {
        tracing::warn!(agent = %profile.inner.name, "{line}");
        eprintln!("warning: {line}");
    }
    // The two scopes this process can see (spec §3.4). The fleet's clock, when
    // there is one, arrives per turn in the A2A `limits` parameter.
    let limits = (boot_global_limits, profile.inner.limits.clone());
    // #1907: denials go to the runner, which routes one naming a turn to that
    // turn's connection only. The clone keeps a handle for `egress/grant`.
    let (denied_tx, denied_rx) = tokio::sync::mpsc::channel(32);
    if let Some(p) = &egress_proxy {
        p.attach_events(denied_tx);
    }
    let egress_for_grants = egress_proxy.clone();
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
            enforcing,
            &boot_cfg,
            ssh_child,
        )
        .await?;
    runner.attach_live(live.clone());
    // The ssh-agent proxy has refused `hitl_unavailable` since the seal; from
    // here its sign requests are asked through gate B.
    if let Some(g) = ssh_gate {
        ssh_gate_boot::fill(
            g,
            ssh_gate_boot::Parts {
                mur_home: &mur_home,
                profile: &profile.inner,
                identity: &identity,
                approvals: pending_approvals.clone(),
                notifier: sock_notif_tx.clone(),
                parking: runner.parking(),
                wait_secs: hitl_timeout_secs,
            },
        );
    }
    // Before any transport starts, so no turn runs without it.
    if let Some(w) = startup_override::waiver(&pending_override, enforcing) {
        warn!(
            nonce = w.nonce(),
            "--no-hitl: default and clamp asks are waived this boot"
        );
        runner.attach_startup_waiver(w);
    }
    mcp_pool.iter().for_each(|p| p.follow(live.profile.clone()));
    runner.clone().drain_egress_denied(denied_rx);
    // Before the first turn: tracks a killed predecessor never closed (#1909).
    let unsettled = runner.report_unsettled_tracks(&mur_home);
    if unsettled > 0 {
        eprintln!(
            "warning: {unsettled} unsettled turn track(s) from a previous run; see `mur open`"
        );
    }
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
        crate::hitl::authority::ApprovalAuthority::new(approval_token, enforcing),
        egress_for_grants,
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
                            data: e.data(),
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

    // 8. Write running.lock (also boot steps 8–10 of the startup override).
    let lock = build_lock(
        &profile,
        lock_transports,
        sandbox_record,
        pending_override,
        override_changes.clone(),
        &agent_home,
    );
    write_lock(&lock_path, &lock)?;
    info!("agent {} ({}) ready", profile.inner.name, profile.inner.id);
    let signer = start_notice::Signer {
        mur_home: &mur_home,
        agent: &profile.inner.name,
        identity: &identity,
        key_version: profile.inner.identity.key_version,
    };
    startup_override::post_banner(&signer, lock.startup_override.as_ref(), &override_changes);

    // 8c. C4 — cron scheduler. Always spawned, even with no entries: it
    //     re-reads `lifecycle.schedule` from the live profile every
    //     RECONCILE_SECS, so a reminder granted while the agent runs — idle or
    //     not — starts without a restart. SIGTERM aborts it with the rest of
    //     transport_tasks.
    {
        let src = live.profile.clone();
        let cs = CronScheduler::live(
            Arc::new(move || src.current().lifecycle.schedule.clone()),
            runner.clone(),
        )
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
            "CronScheduler started (live)"
        );
    }

    // 8d. C6 — idle scheduler. Wakes every 30 s, re-reads the triggers from the
    //     live profile, fires when idle >= IdleTrigger.after_secs.
    {
        let src = live.profile.clone();
        let is = IdleScheduler::live(
            Arc::new(move || src.current().lifecycle.idle_triggers.clone()),
            runner.clone(),
            profile.inner.companion.proactive.quiet_hours.clone(),
        );
        transport_tasks.push(is.spawn());
        info!(
            count = profile.inner.lifecycle.idle_triggers.len(),
            "IdleScheduler started (live)"
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

    // 8b. Companion subsystem (Phase 1.1 M5.7) — see `bootstrap::spawn_companion`.
    if let Some(t) = bootstrap::spawn_companion(&profile.inner, &agent_home, llm_for_companion) {
        transport_tasks.push(t);
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
    // One deadline for the whole stop; the MCP stop gets a reserved slice so
    // a browser can export its login even with a turn in flight (#2360).
    let stop_secs = profile.inner.lifecycle.stop_timeout_secs;
    let plan = stop_budget::StopPlan::new(tokio::time::Instant::now(), stop_secs);
    stop_budget::run(plan, stop_secs, &runner, mcp_pool.as_deref()).await;
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
    lock_handle.release();
    Ok(())
}
