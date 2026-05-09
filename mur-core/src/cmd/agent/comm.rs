//! A2A forwarders — `mur agent send` and `mur agent card`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use mur_common::LockFile;

use super::{resolve_mur_home, resolve_runtime_target};

pub fn cmd_send(name: &str, message_json: &str) -> Result<()> {
    let msg: serde_json::Value =
        serde_json::from_str(message_json).context("parse --message JSON")?;
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/send",
        "params": {"message": msg}
    });
    let result = dial_rpc(name, &req)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub fn cmd_card(name: &str) -> Result<()> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "agent/card"
    });
    let mur_home = resolve_mur_home()?;
    let lock_path = mur_home.join("agents").join(name).join("running.lock");
    let result = if lock_path.exists() {
        dial_rpc(name, &req)?
    } else {
        ephemeral_card_via_runtime(name, &mur_home)?
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// When the target agent isn't already running, spawn the runtime in
/// stdio mode just long enough to ask for `agent/card`, then SIGTERM it.
fn ephemeral_card_via_runtime(name: &str, mur_home: &Path) -> Result<serde_json::Value> {
    let runtime = resolve_runtime_target();
    if !runtime.is_absolute() && !runtime.exists() {
        bail!(
            "agent '{name}' not running and runtime binary not found (set MUR_AGENT_RUNTIME_BIN)"
        );
    }

    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let mut child = std::process::Command::new(&runtime)
        .env("MUR_HOME", mur_home)
        .args(["--profile", name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", runtime.display()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin on spawned runtime"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no stdout on spawned runtime"))?;

    let req = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"agent/card\"}\n";
    stdin.write_all(req).context("write to runtime stdin")?;
    drop(stdin);

    let reader = BufReader::new(stdout);
    let mut found = None;
    for line in reader.lines() {
        let line = line.context("read runtime stdout")?;
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("id") == Some(&serde_json::json!(1)) {
            found = Some(v.get("result").cloned().unwrap_or(serde_json::Value::Null));
            break;
        }
    }

    // Best-effort SIGTERM the ephemeral runtime.
    #[cfg(unix)]
    {
        let pid = child.id();
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    let _ = child.wait();

    found.ok_or_else(|| anyhow!("ephemeral runtime did not produce a card response"))
}

fn dial_rpc(name: &str, req: &serde_json::Value) -> Result<serde_json::Value> {
    let mur_home = resolve_mur_home()?;
    let lock_path = mur_home.join("agents").join(name).join("running.lock");
    let bytes = fs::read(&lock_path)
        .with_context(|| format!("agent '{name}' is not running (no {})", lock_path.display()))?;
    let lock: LockFile = serde_json::from_slice(&bytes).context("parse running.lock")?;
    let sock = lock
        .transports
        .unix_socket
        .ok_or_else(|| anyhow!("agent '{name}' has no unix-socket transport"))?;

    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(&sock)
            .with_context(|| format!("connect {sock}"))?;
        let line = format!("{}\n", serde_json::to_string(req)?);
        stream.write_all(line.as_bytes())?;
        let reader = BufReader::new(stream.try_clone()?);
        for line in reader.lines() {
            let line = line.context("read response line")?;
            let v: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id") == Some(&req["id"]) {
                if let Some(err) = v.get("error") {
                    bail!("agent error: {err}");
                }
                return Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null));
            }
            // Ignore notifications (no matching id).
        }
        bail!("EOF before matching response");
    }
    #[cfg(not(unix))]
    {
        let _ = sock;
        bail!("unix socket transport is only supported on unix hosts")
    }
}
