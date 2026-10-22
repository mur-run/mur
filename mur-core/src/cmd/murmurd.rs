use anyhow::{Context, Result, bail};

use crate::cmd::init_daemon::Supervision;

pub fn cmd_murmurd_status() -> Result<()> {
    let lock_path = mur_common::home::mur_home_or_err()?.join("murmurd.lock");

    match std::fs::read_to_string(&lock_path) {
        Ok(s) => {
            let state: serde_json::Value = serde_json::from_str(&s)?;
            let pid = state.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
            let hb = state
                .get("heartbeat_at")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!("murmurd running (pid {pid}, last heartbeat {hb})");
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("murmurd not running (no lock file)");
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub fn cmd_murmurd_stop() -> Result<()> {
    // A supervised daemon is stopped THROUGH its service manager: a bare
    // SIGTERM is undone by KeepAlive / Restart=always a few seconds later.
    if let Supervision::Owns(service) = Supervision::detect() {
        service.stop()?;
        // Also reaps a lock holder the service does not own (an earlier
        // `mur daemon start` spawn), waits for exit, and clears the lock.
        stop_lock_holder()?;
        println!(
            "murmurd stopped via {service}; it stays down until the next login \
             or `mur daemon start`"
        );
        return Ok(());
    }

    let lock_path = mur_common::home::mur_home_or_err()?.join("murmurd.lock");

    let s = match std::fs::read_to_string(&lock_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("murmurd not running");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let state: serde_json::Value = serde_json::from_str(&s)?;
    let pid = state
        .get("pid")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("malformed lock file"))?;

    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .spawn();
    }

    let _ = std::fs::remove_file(&lock_path);
    println!("murmurd stopped (sent SIGTERM to pid {pid})");
    Ok(())
}

/// The murmurd binary this `mur` would launch: a sibling of the current exe.
fn murmurd_binary() -> Result<std::path::PathBuf> {
    let murmurd = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("murmurd")))
        .unwrap_or_else(|| std::path::PathBuf::from("murmurd"));
    if !murmurd.exists() {
        anyhow::bail!(
            "murmurd binary not found at {}. Build with: cargo build -p mur-daemon",
            murmurd.display()
        );
    }
    Ok(murmurd)
}

pub fn cmd_murmurd_start(detach: bool) -> Result<()> {
    // A supervised daemon must be started through its service manager: a
    // directly spawned one takes the singleton lock and the supervised job
    // then crash-loops on "already running" every ThrottleInterval.
    if detach {
        match Supervision::detect() {
            Supervision::Owns(service) => {
                service.start(false)?;
                println!("murmurd started via {service}");
                return Ok(());
            }
            other => note_foreign_service(&other),
        }
    }
    let murmurd = murmurd_binary()?;

    let mut cmd = std::process::Command::new(&murmurd);
    if detach {
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd.spawn()?;
        println!("murmurd started in background");
    } else {
        let status = cmd.status()?;
        if !status.success() {
            anyhow::bail!("murmurd exited with: {status}");
        }
    }
    Ok(())
}

/// How long `restart` waits for the old daemon to exit before SIGKILL. The
/// daemon's shutdown is quick (drop sockets, flush lock); this only guards a
/// wedged process.
const MURMURD_STOP_WAIT_SECS: u64 = 15;

fn murmurd_lock_path() -> Result<std::path::PathBuf> {
    Ok(mur_common::home::mur_home_or_err()?.join("murmurd.lock"))
}

/// Whether a murmurd instance is currently alive (lock exists AND its pid is).
pub fn murmurd_running() -> bool {
    let Ok(path) = murmurd_lock_path() else {
        return false;
    };
    let Ok(s) = std::fs::read_to_string(&path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&s)
        .ok()
        .and_then(|v| v.get("pid").and_then(|p| p.as_u64()))
        .is_some_and(|pid| mur_common::lock_file::pid_alive(pid as u32))
}

/// `mur daemon restart` — stop the running daemon, WAIT for its pid to
/// actually exit (plain `stop` returns the moment SIGTERM is sent, and a
/// back-to-back start would race the dying process for the lock), then start
/// a fresh one detached. The way a running daemon moves onto an upgraded
/// binary — without this, murmurd keeps executing pre-upgrade code
/// indefinitely.
pub fn cmd_murmurd_restart() -> Result<()> {
    // Resolve the replacement binary FIRST: stop-then-fail-to-start leaves
    // the daemon down, which a smoke run demonstrated the hard way.
    murmurd_binary()?;
    match Supervision::detect() {
        Supervision::Owns(service) => {
            // Also stop a lock holder the service does not own (e.g. one an
            // earlier `mur daemon start` spawned): `kickstart -k` only
            // replaces the service's own instance, and an orphan would keep
            // the lock and the respawned job crash-looping.
            stop_lock_holder()?;
            service.start(true)?;
            println!("murmurd restarted via {service}");
            Ok(())
        }
        // `Other`: the registered service runs a different MUR home's
        // daemon — never kickstart it; manage this home's daemon directly
        // (`cmd_murmurd_start` prints why).
        Supervision::Other { .. } | Supervision::None => {
            stop_lock_holder()?;
            cmd_murmurd_start(true)
        }
    }
}

/// Say so when a murmurd service exists but serves another MUR home, so a
/// direct start for this home is visibly a choice, not a missed service.
fn note_foreign_service(s: &Supervision) {
    if let Supervision::Other {
        service,
        service_home,
    } = s
    {
        let here = mur_common::home::mur_home_lossy();
        println!(
            "note: the {service} murmurd service serves {}, not {}; \
             starting an unsupervised daemon for {} (reinstall the service \
             with `mur init` under this MUR_HOME to supervise it)",
            service_home.display(),
            here.display(),
            here.display()
        );
    }
}

/// SIGTERM the pid in `murmurd.lock`, wait for it to exit (SIGKILL after
/// [`MURMURD_STOP_WAIT_SECS`]), then remove the lock. A missing lock is
/// "nothing to stop"; an unreadable one is an error, never "not running".
fn stop_lock_holder() -> Result<()> {
    let lock_path = murmurd_lock_path()?;
    match std::fs::read_to_string(&lock_path) {
        Ok(s) => {
            let pid = serde_json::from_str::<serde_json::Value>(&s)
                .context("murmurd.lock contains invalid JSON; refusing to restart blind")?
                .get("pid")
                .and_then(|p| p.as_u64())
                .context("murmurd.lock has no 'pid' field; refusing to restart blind")?
                as u32;
            if mur_common::lock_file::pid_alive(pid) {
                #[cfg(unix)]
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_secs(MURMURD_STOP_WAIT_SECS);
                while std::time::Instant::now() < deadline && mur_common::lock_file::pid_alive(pid)
                {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if mur_common::lock_file::pid_alive(pid) {
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                // Verify the kill actually took effect before we tell the
                // caller it's safe to start a fresh instance — a wedged
                // (D-state) process can survive SIGKILL, and starting a
                // second daemon on top of a still-live one races both for
                // the same lock and socket.
                if mur_common::lock_file::pid_alive(pid) {
                    bail!(
                        "murmurd (pid {pid}) did not exit after SIGTERM/SIGKILL; \
                         refusing to start a second instance. Investigate the \
                         process manually (it may be wedged) before retrying."
                    );
                }
                println!("murmurd stopped (pid {pid})");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No lock file at all: nothing to stop, safe to proceed.
        }
        Err(e) => {
            // Lock file exists but couldn't be read (permissions, transient
            // I/O, truncated concurrent write). Treat this as "unknown
            // state" rather than "not running" — starting a new daemon here
            // could spawn a second instance alongside a live one.
            return Err(e).context(format!(
                "could not read murmurd lock at {}; refusing to restart blind \
                 (daemon may still be running)",
                lock_path.display()
            ));
        }
    }
    // Only remove the lock once we've either confirmed the old daemon is
    // dead or confirmed there was none to begin with.
    if let Err(e) = std::fs::remove_file(&lock_path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        println!(
            "warning: could not remove stale lock at {}: {e}",
            lock_path.display()
        );
    }
    Ok(())
}
