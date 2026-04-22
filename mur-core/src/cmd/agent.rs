//! `mur agent` subcommands — create/list/status/... (Task 22-30).
//! P0a: just `create`.

use anyhow::{Context, Result, anyhow, bail};
use mur_common::agent::*;
use mur_common::{AgentProfile as _AgentProfile, LockFile};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

pub fn cmd_create(
    name: &str,
    _no_interactive: bool,
    display_name: Option<String>,
    model: Option<String>,
) -> Result<()> {
    validate_name(name)?;

    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if agent_home.exists() {
        bail!("agent {name} already exists at {}", agent_home.display());
    }
    fs::create_dir_all(&agent_home).with_context(|| format!("create {}", agent_home.display()))?;

    let display_name = display_name.unwrap_or_else(|| name.to_string());
    let model = model.unwrap_or_else(|| "llama3.2:3b".to_string());
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::now_v7().to_string();

    let profile = AgentProfile {
        schema: 1,
        id,
        name: name.to_string(),
        display_name,
        version: "0.1.0".to_string(),
        persona: Persona {
            category: PersonaCategory::Custom,
            description: format!("Agent {name}"),
            traits: PersonaTraits {
                tone: "concise".into(),
                risk: "cautious".into(),
                verbosity: "medium".into(),
            },
        },
        sys_prompt_file: "sys_prompt.md".into(),
        model: ModelConfig {
            provider: "ollama".into(),
            name: model,
            params: BTreeMap::new(),
        },
        mcp_servers: vec![],
        skills: vec![],
        transport: TransportConfig {
            stdio: true,
            socket: SocketTransportConfig {
                enabled: true,
                bind: format!("unix://{}/agent.sock", agent_home.display()),
                auth: None,
            },
        },
        communication: CommunicationConfig {
            accepts_from: vec!["*".into()],
            sends_to: vec![],
        },
        capabilities: vec!["a2a.message.send".into(), "a2a.tasks".into()],
        entitlements: default_entitlements_custom(),
        notifications: NotificationsConfig::default(),
        retry: RetryConfig {
            llm: RetryPolicy {
                max_retries: 3,
                backoff: BackoffStrategy::Exponential,
                initial_delay_ms: 1000,
                max_delay_ms: Some(30000),
                retry_on: vec!["rate_limit".into()],
            },
            tool: RetryPolicy {
                max_retries: 1,
                backoff: BackoffStrategy::Fixed,
                initial_delay_ms: 500,
                max_delay_ms: None,
                retry_on: vec![],
            },
        },
        lifecycle: LifecycleConfig {
            restart: RestartPolicy::OnFailure,
            max_restarts: 3,
            restart_window_secs: 600,
            stop_timeout_secs: 15,
            mcp_required: false,
        },
        created_at: now.clone(),
        updated_at: now,
    };

    let yaml = serde_yaml_ng::to_string(&profile).context("serialize profile.yaml")?;
    write_atomic(&agent_home.join("profile.yaml"), yaml.as_bytes())?;
    write_atomic(
        &agent_home.join("sys_prompt.md"),
        format!("# {name}\n\nYou are an assistant.\n").as_bytes(),
    )?;

    let bin_dir = resolve_bin_dir()?;
    fs::create_dir_all(&bin_dir).with_context(|| format!("create {}", bin_dir.display()))?;
    let symlink = bin_dir.join(format!("mur_agent_{name}"));
    if symlink.symlink_metadata().is_ok() {
        fs::remove_file(&symlink)?;
    }
    let target = resolve_runtime_target();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &symlink)
        .with_context(|| format!("symlink {} -> {}", symlink.display(), target.display()))?;
    #[cfg(windows)]
    std::fs::copy(&target, &symlink)
        .with_context(|| format!("copy {} to {}", target.display(), symlink.display()))?;

    println!("Created agent '{}' at {}", name, agent_home.display());
    println!("Symlink: {} -> {}", symlink.display(), target.display());
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("agent name must not be empty");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("agent name must match [A-Za-z0-9_]+");
    }
    Ok(())
}

fn resolve_mur_home() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("MUR_HOME") {
        return Ok(PathBuf::from(v));
    }
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("no home dir"))?
        .join(".mur"))
}

fn resolve_bin_dir() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("MUR_AGENT_BIN_DIR") {
        return Ok(PathBuf::from(v));
    }
    if let Some(home) = dirs::home_dir() {
        return Ok(home.join(".local/bin"));
    }
    bail!("cannot resolve bin dir")
}

fn resolve_runtime_target() -> PathBuf {
    if let Some(v) = std::env::var_os("MUR_AGENT_RUNTIME_BIN") {
        return PathBuf::from(v);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("mur-agent-runtime");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("mur-agent-runtime")
}

fn default_entitlements_custom() -> Entitlements {
    Entitlements {
        network: NetworkEntitlement {
            inbound: InboundNetwork { ports: vec![] },
            outbound: OutboundNetwork {
                mode: NetworkOutboundMode::Restricted,
                allow_hosts: vec![],
                protocols: vec!["tcp".into()],
                resolve_dns: ResolveDnsConfig::default(),
            },
        },
        filesystem: FilesystemEntitlement {
            read: vec![],
            write: vec![],
            deny: vec!["~/.ssh".into(), "~/.aws".into()],
        },
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement {
                mode: SpawnMode::Allowlist,
                allowed: vec![],
            },
        },
        syscalls: SyscallsEntitlement {
            mode: "default".into(),
            extra_deny: vec![],
        },
        limits: LimitsEntitlement {
            cpu_seconds: None,
            memory_mb: 512,
            file_descriptors: 1024,
            processes: 32,
        },
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

// ─── list / status ───────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
struct AgentRow {
    name: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uptime: Option<String>,
    category: String,
}

pub fn cmd_list(json: bool) -> Result<()> {
    let rows = collect_agents()?;
    if json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        println!(
            "{:<20} {:<10} {:<20} {:<10} {:<12}",
            "NAME", "STATUS", "UPTIME", "PID", "CATEGORY"
        );
        for r in &rows {
            let pid = r
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string());
            let uptime = r.uptime.clone().unwrap_or_else(|| "-".to_string());
            println!(
                "{:<20} {:<10} {:<20} {:<10} {:<12}",
                r.name, r.status, uptime, pid, r.category
            );
        }
    }
    Ok(())
}

pub fn cmd_status(name: &str) -> Result<()> {
    let rows = collect_agents()?;
    let row = rows
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| anyhow!("agent '{name}' not found"))?;
    println!("● {} - {}", row.name, row.category);
    println!(
        "   Loaded: {}",
        resolve_mur_home()?.join("agents").join(name).display()
    );
    println!("   Active: {}", row.status);
    if let Some(pid) = row.pid {
        println!("     Main PID: {pid}");
    }
    if let Some(up) = &row.uptime {
        println!("       Uptime: {up}");
    }
    Ok(())
}

fn collect_agents() -> Result<Vec<AgentRow>> {
    let mur_home = resolve_mur_home()?;
    let agents_dir = mur_home.join("agents");
    let mut rows = Vec::new();
    let entries = match fs::read_dir(&agents_dir) {
        Ok(e) => e,
        Err(_) => return Ok(rows),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let profile_path = dir.join("profile.yaml");
        if !profile_path.exists() {
            continue;
        }
        let yaml = fs::read_to_string(&profile_path)
            .with_context(|| format!("read {}", profile_path.display()))?;
        let profile: _AgentProfile = serde_yaml_ng::from_str(&yaml)
            .with_context(|| format!("parse {}", profile_path.display()))?;

        let lock_path = dir.join("running.lock");
        let (status, pid, uptime) = classify(&lock_path);
        rows.push(AgentRow {
            name: profile.name,
            status,
            pid,
            uptime,
            category: format!("{:?}", profile.persona.category).to_lowercase(),
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

fn classify(lock_path: &Path) -> (&'static str, Option<u32>, Option<String>) {
    if !lock_path.exists() {
        return ("stopped", None, None);
    }
    let bytes = match fs::read(lock_path) {
        Ok(b) => b,
        Err(_) => return ("stale", None, None),
    };
    let lock: LockFile = match serde_json::from_slice(&bytes) {
        Ok(l) => l,
        Err(_) => return ("stale", None, None),
    };
    if !pid_alive(lock.pid) {
        return ("stale", Some(lock.pid), None);
    }
    let uptime = chrono::DateTime::parse_from_rfc3339(&lock.started_at)
        .ok()
        .map(|start| {
            let secs = (chrono::Utc::now() - start.with_timezone(&chrono::Utc))
                .num_seconds()
                .max(0);
            format!("{}s", secs)
        });
    ("running", Some(lock.pid), uptime)
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

// ─── stop / remove / rename ──────────────────────────────────────────────

pub fn cmd_stop(name: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    let lock_path = agent_home.join("running.lock");
    if !lock_path.exists() {
        bail!("agent '{name}' is not running");
    }
    let bytes = fs::read(&lock_path).with_context(|| format!("read {}", lock_path.display()))?;
    let lock: LockFile =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", lock_path.display()))?;

    // Load stop_timeout_secs from profile, default 15.
    let timeout = {
        let pp = agent_home.join("profile.yaml");
        fs::read_to_string(&pp)
            .ok()
            .and_then(|y| serde_yaml_ng::from_str::<_AgentProfile>(&y).ok())
            .map(|p| p.lifecycle.stop_timeout_secs)
            .unwrap_or(15)
    };

    #[cfg(unix)]
    unsafe {
        libc::kill(lock.pid as libc::pid_t, libc::SIGTERM);
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    while std::time::Instant::now() < deadline {
        if !pid_alive(lock.pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if pid_alive(lock.pid) {
        #[cfg(unix)]
        unsafe {
            libc::kill(lock.pid as libc::pid_t, libc::SIGKILL);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // Supervisor normally removes the lock, but after SIGKILL nothing will —
    // or our sleep-fixture has no lock cleanup — so best-effort remove here.
    let _ = fs::remove_file(&lock_path);
    println!("Stopped agent '{name}'");
    Ok(())
}

pub fn cmd_remove(name: &str, purge: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    refuse_if_running(&agent_home, name)?;

    let bin_dir = resolve_bin_dir()?;
    let symlink = bin_dir.join(format!("mur_agent_{name}"));
    if symlink.symlink_metadata().is_ok() {
        fs::remove_file(&symlink).ok();
    }

    if purge {
        fs::remove_dir_all(&agent_home)
            .with_context(|| format!("remove_dir_all {}", agent_home.display()))?;
        println!("Purged agent '{name}'");
    } else {
        println!(
            "Removed agent '{name}' (data preserved at {})",
            agent_home.display()
        );
    }
    Ok(())
}

pub fn cmd_rename(old: &str, new: &str) -> Result<()> {
    validate_name(new)?;
    let mur_home = resolve_mur_home()?;
    let old_home = mur_home.join("agents").join(old);
    let new_home = mur_home.join("agents").join(new);
    if !old_home.exists() {
        bail!("agent '{old}' not found");
    }
    if new_home.exists() {
        bail!("agent '{new}' already exists");
    }
    refuse_if_running(&old_home, old)?;

    // Update profile.yaml name + updated_at before renaming the directory
    // so the on-disk value matches the directory once the rename completes.
    let profile_path = old_home.join("profile.yaml");
    let yaml = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: _AgentProfile = serde_yaml_ng::from_str(&yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;
    profile.name = new.to_string();
    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let new_yaml = serde_yaml_ng::to_string(&profile).context("serialize profile.yaml")?;
    write_atomic(&profile_path, new_yaml.as_bytes())?;

    fs::rename(&old_home, &new_home)
        .with_context(|| format!("rename {} -> {}", old_home.display(), new_home.display()))?;

    let bin_dir = resolve_bin_dir()?;
    let old_symlink = bin_dir.join(format!("mur_agent_{old}"));
    let new_symlink = bin_dir.join(format!("mur_agent_{new}"));
    if old_symlink.symlink_metadata().is_ok() {
        let target =
            fs::read_link(&old_symlink).unwrap_or_else(|_| PathBuf::from("mur-agent-runtime"));
        fs::remove_file(&old_symlink).ok();
        if new_symlink.symlink_metadata().is_ok() {
            fs::remove_file(&new_symlink).ok();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &new_symlink).with_context(|| {
            format!("symlink {} -> {}", new_symlink.display(), target.display())
        })?;
        #[cfg(windows)]
        std::fs::copy(&target, &new_symlink)
            .with_context(|| format!("copy {} to {}", target.display(), new_symlink.display()))?;
    }

    println!("Renamed '{old}' -> '{new}'");
    Ok(())
}

fn refuse_if_running(agent_home: &Path, name: &str) -> Result<()> {
    let lock_path = agent_home.join("running.lock");
    if !lock_path.exists() {
        return Ok(());
    }
    let bytes = fs::read(&lock_path).with_context(|| format!("read {}", lock_path.display()))?;
    if let Ok(lock) = serde_json::from_slice::<LockFile>(&bytes)
        && pid_alive(lock.pid)
    {
        bail!("agent '{name}' is running; stop it first");
    }
    Ok(())
}

// ─── send / card (A2A forwarders) ────────────────────────────────────────

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
    let result = dial_rpc(name, &req)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
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

// ─── install-service (launchd / systemd generator) ───────────────────────

pub fn cmd_install_service(name: &str, dry_run: bool) -> Result<()> {
    // Confirm the agent exists so we fail fast on typos.
    let mur_home = resolve_mur_home()?;
    if !mur_home.join("agents").join(name).exists() {
        bail!("agent '{name}' not found under {}", mur_home.display());
    }
    let bin_dir = resolve_bin_dir()?;
    let symlink = bin_dir.join(format!("mur_agent_{name}"));

    #[cfg(target_os = "macos")]
    {
        let plist = darwin_plist(name, &symlink);
        if dry_run {
            print!("{plist}");
            return Ok(());
        }
        let dest = dirs::home_dir()
            .ok_or_else(|| anyhow!("no home dir"))?
            .join(format!("Library/LaunchAgents/run.mur.agent.{name}.plist"));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&dest, plist.as_bytes())?;
        let _ = std::process::Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&dest)
            .status();
        println!("Installed launchd service at {}", dest.display());
    }
    #[cfg(target_os = "linux")]
    {
        let unit = linux_unit(name, &symlink);
        if dry_run {
            print!("{unit}");
            return Ok(());
        }
        let dest = dirs::config_dir()
            .ok_or_else(|| anyhow!("no config dir"))?
            .join(format!("systemd/user/mur-agent-{name}.service"));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&dest, unit.as_bytes())?;
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now"])
            .arg(format!("mur-agent-{name}.service"))
            .status();
        println!("Installed systemd --user unit at {}", dest.display());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (name, dry_run, symlink);
        bail!("install-service only supports macOS (launchd) and Linux (systemd --user)")
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn darwin_plist(name: &str, symlink: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>run.mur.agent.{name}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{sym}</string>
        <string>start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>/tmp/mur-agent-{name}.err.log</string>
    <key>StandardOutPath</key>
    <string>/tmp/mur-agent-{name}.out.log</string>
</dict>
</plist>
"#,
        sym = symlink.display(),
    )
}

// ─── mcp add/list/remove/rename ──────────────────────────────────────────

fn load_profile_for_edit(name: &str) -> Result<(PathBuf, _AgentProfile)> {
    let mur_home = resolve_mur_home()?;
    let path = mur_home.join("agents").join(name).join("profile.yaml");
    if !path.exists() {
        bail!("agent '{name}' not found");
    }
    let yaml = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let profile: _AgentProfile =
        serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", path.display()))?;
    Ok((path, profile))
}

fn save_profile(path: &Path, profile: &mut _AgentProfile) -> Result<()> {
    profile.updated_at = chrono::Utc::now().to_rfc3339();
    let yaml = serde_yaml_ng::to_string(profile).context("serialize profile.yaml")?;
    write_atomic(path, yaml.as_bytes())
}

pub fn cmd_mcp_list(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.is_empty() {
        println!("(no MCP servers configured)");
        return Ok(());
    }
    for s in &profile.mcp_servers {
        println!("{}\t{} {}", s.name, s.command, s.args.join(" "));
    }
    Ok(())
}

pub fn cmd_mcp_add(name: &str, server_id: &str, command: &str, args: &[String]) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.iter().any(|s| s.name == server_id) {
        bail!("MCP server '{server_id}' already exists on '{name}'");
    }
    profile.mcp_servers.push(McpServerEntry {
        name: server_id.to_string(),
        command: command.to_string(),
        args: args.to_vec(),
    });
    // Sync spawn allowlist so the supervisor is permitted to launch this MCP.
    if !profile
        .entitlements
        .processes
        .spawn
        .allowed
        .iter()
        .any(|a| a == command)
    {
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .push(command.to_string());
    }
    save_profile(&path, &mut profile)
}

pub fn cmd_mcp_remove(name: &str, server_id: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let before = profile.mcp_servers.len();
    let removed_command = profile
        .mcp_servers
        .iter()
        .find(|s| s.name == server_id)
        .map(|s| s.command.clone());
    profile.mcp_servers.retain(|s| s.name != server_id);
    if profile.mcp_servers.len() == before {
        bail!("MCP server '{server_id}' not found on '{name}'");
    }
    // Drop the command from the spawn allowlist only if no other mcp entry
    // still needs it.
    if let Some(cmd) = removed_command
        && !profile.mcp_servers.iter().any(|s| s.command == cmd)
    {
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .retain(|a| a != &cmd);
    }
    save_profile(&path, &mut profile)
}

pub fn cmd_mcp_rename(name: &str, old: &str, new: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.iter().any(|s| s.name == new) {
        bail!("MCP server '{new}' already exists on '{name}'");
    }
    let hit = profile.mcp_servers.iter_mut().find(|s| s.name == old);
    match hit {
        Some(s) => s.name = new.to_string(),
        None => bail!("MCP server '{old}' not found on '{name}'"),
    }
    save_profile(&path, &mut profile)
}

// ─── prompt show/edit/set ────────────────────────────────────────────────

fn prompt_path_for(name: &str) -> Result<PathBuf> {
    let mur_home = resolve_mur_home()?;
    let dir = mur_home.join("agents").join(name);
    if !dir.exists() {
        bail!("agent '{name}' not found");
    }
    Ok(dir.join("sys_prompt.md"))
}

pub fn cmd_prompt_show(name: &str) -> Result<()> {
    let path = prompt_path_for(name)?;
    let body = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    // Print without an implicit trailing newline to keep show byte-exact.
    print!("{body}");
    Ok(())
}

pub fn cmd_prompt_edit(name: &str) -> Result<()> {
    let path = prompt_path_for(name)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("spawn editor '{editor}'"))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    Ok(())
}

pub fn cmd_prompt_set(name: &str, content: Option<&str>, file: Option<&str>) -> Result<()> {
    let path = prompt_path_for(name)?;
    let new_bytes: Vec<u8> = match (content, file) {
        (_, Some(p)) => fs::read(p).with_context(|| format!("read {p}"))?,
        (Some(s), None) => s.as_bytes().to_vec(),
        (None, None) => bail!("provide either inline CONTENT or -f FILE"),
    };

    // Preserve previous value as sys_prompt.md.bak before overwriting.
    if path.exists() {
        let bak = path.with_extension("md.bak");
        fs::copy(&path, &bak)
            .with_context(|| format!("backup {} -> {}", path.display(), bak.display()))?;
    }
    write_atomic(&path, &new_bytes)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_unit(name: &str, symlink: &Path) -> String {
    format!(
        r#"[Unit]
Description=murmur agent {name}
After=default.target

[Service]
Type=simple
ExecStart={sym} start
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
"#,
        sym = symlink.display(),
    )
}
