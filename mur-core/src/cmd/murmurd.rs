use anyhow::Result;

pub fn cmd_murmurd_status() -> Result<()> {
    let lock_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur")
        .join("murmurd.lock");

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
    let lock_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur")
        .join("murmurd.lock");

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

pub fn cmd_murmurd_start(detach: bool) -> Result<()> {
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
