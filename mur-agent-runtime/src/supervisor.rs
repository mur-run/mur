//! Agent runtime entrypoint — assembles profile, dispatcher, telemetry, and
//! drives the stdio (and optionally Unix-socket) transports until SIGTERM.

use crate::entitlements::detect_warnings;
use crate::lock_file::{LockHandle, write_lock};
use crate::multi_call::{DispatchError, extract_profile_name, verify_name_match};
use crate::profile::Profile;
use crate::protocol::a2a_server::Dispatcher;
use crate::protocol::methods::{
    card::CardHandler,
    message_send::MessageSendHandler,
    tasks::{TasksCancelHandler, TasksGetHandler, TasksListHandler},
};
use crate::socket_path::resolve_bind_target;
use crate::task_runner::TaskRunner;
use crate::telemetry_writer::TelemetryWriter;
use crate::transport::{stdio::serve_stdio, unix_socket::serve_unix};
use mur_common::{LockFile, agent::LockTransports};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};

pub async fn entrypoint() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    // 1. Determine profile name from argv[0] (or --profile)
    let argv0 = std::env::args().next().unwrap_or_default();
    let name = match extract_profile_name(&argv0) {
        Ok(n) => n,
        Err(DispatchError::BareRuntime) => read_flag_profile_from_args()?,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    // 2. Resolve agent_home and load profile
    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"));
    let agent_home = mur_home.join("agents").join(&name);
    let profile = match Profile::load(&agent_home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error[profile_invalid]: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = verify_name_match(&name, &profile.inner.name) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }

    // 3. Warn on loose entitlements
    for w in detect_warnings(&profile.inner) {
        warn!(kind = ?w.kind, "{}", w.message);
    }

    // 4. Spawn telemetry writer
    let (writer, notif_rx) = TelemetryWriter::new(
        agent_home.join("telemetry"),
        profile.inner.name.clone(),
        profile.inner.id.clone(),
    )
    .await?;

    // 5. Acquire running.lock
    let lock_path = agent_home.join("running.lock");
    let _lock_handle =
        LockHandle::acquire(&lock_path).map_err(|e| anyhow::anyhow!("already running ({e})"))?;

    // 6. Build dispatcher (shared Arc so multiple transports can read it)
    let profile_arc = Arc::new(profile.clone());
    let runner = Arc::new(TaskRunner::new_stub_echo()); // Task 21 swaps to real backend
    let dispatcher = Arc::new(build_dispatcher(&profile_arc, &runner));

    // 7. Transports
    let mut transport_tasks = vec![];
    let mut lock_transports = LockTransports {
        stdio: profile.inner.transport.stdio,
        unix_socket: None,
        tcp: None,
    };

    // Decide how to route telemetry notifications. Socket (if present) gets
    // them so every connected peer can subscribe; otherwise stdio does.
    let socket_enabled = profile.inner.transport.socket.enabled
        && profile.inner.transport.socket.bind.starts_with("unix://");
    let (stdio_notif_tx, stdio_notif_rx) = tokio::sync::mpsc::channel(256);
    let (sock_notif_tx, sock_notif_rx) = tokio::sync::mpsc::channel(256);

    transport_tasks.push(tokio::spawn(async move {
        let mut notif = notif_rx;
        while let Some(n) = notif.recv().await {
            if socket_enabled {
                let _ = sock_notif_tx.send(n).await;
            } else {
                let _ = stdio_notif_tx.send(n).await;
            }
        }
    }));

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
    // Keep `writer` alive so the telemetry fan-out mpsc isn't closed.
    let _writer = writer;

    // 9. Drive stdio in the foreground so SIGTERM can unblock the process
    //    even while stdin is idle.
    if profile.inner.transport.stdio {
        let d = dispatcher.clone();
        transport_tasks.push(tokio::spawn(async move {
            let _ = serve_stdio(d, tokio::io::stdin(), tokio::io::stdout(), stdio_notif_rx).await;
        }));
    }

    // 10. Wait for SIGTERM / SIGINT
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = sigterm.recv() => info!("SIGTERM received"),
        _ = sigint.recv() => info!("SIGINT received"),
    }
    for t in transport_tasks {
        t.abort();
    }
    Ok(())
}

fn build_dispatcher(profile: &Arc<Profile>, runner: &Arc<TaskRunner>) -> Dispatcher {
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
