use super::*;

// ─── list / status ───────────────────────────────────────────────────────

/// Trailing annotation appended to human-readable output when a running agent's
/// binary is outdated.  Kept out of --json output.
const STALE_SUFFIX: &str = " — stale runtime (restart to apply)";

#[derive(Debug, serde::Serialize)]
struct AgentRow {
    name: String,
    status: &'static str,
    // Derived visual marker for `status`; CLI table only — kept out of the
    // `--json` payload (PM scope: only the human table gains the column).
    #[serde(skip)]
    emoji: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uptime: Option<String>,
    category: String,
}

/// Structured agent list entry — returned by do_list().
/// Public API consumed by mur-mcp-server.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentListEntry {
    pub name: String,
    pub running: bool,
    pub transport: String,
}

/// Structured agent status — returned by do_status().
/// Public API consumed by mur-mcp-server.
#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentStatusInfo {
    pub name: String,
    pub running: bool,
    pub pid: Option<u32>,
    pub transport: String,
    pub socket_path: Option<String>,
    pub skills_count: usize,
    pub mcp_servers_count: usize,
}

#[allow(dead_code)]
pub fn do_list() -> Result<Vec<AgentListEntry>> {
    let home = super::resolve_mur_home()?;
    let agents_dir = home.join("agents");
    if !agents_dir.exists() {
        return Ok(vec![]);
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&agents_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let profile_path = entry.path().join("profile.yaml");
            // Only real agent directories have a profile.yaml. Without this guard,
            // non-agent entries under ~/.mur/agents (e.g. the `.git` dir when that
            // folder is a git repo, or legacy dirs like `Author/` with no
            // profile.yaml) get listed as phantom agents. Matches the CLI's
            // `collect_agents` filter so `mur_agent_status` and `mur agent list` agree.
            if !profile_path.exists() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let running = check_running(&entry.path());
            let transport = load_transport(&profile_path).unwrap_or_else(|_| "stdio".into());
            entries.push(AgentListEntry {
                name,
                running,
                transport,
            });
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

#[allow(dead_code)]
pub fn do_status(name: &str) -> Result<AgentStatusInfo> {
    let home = super::resolve_mur_home()?;
    let agent_dir = home.join("agents").join(name);
    if !agent_dir.exists() {
        anyhow::bail!(
            "Agent '{}' not found. Run 'mur agent list' to see configured agents.",
            name
        );
    }
    let profile_path = agent_dir.join("profile.yaml");
    let running = check_running(&agent_dir);
    let pid = get_pid(&agent_dir);
    let transport = if profile_path.exists() {
        load_transport(&profile_path).unwrap_or_else(|_| "stdio".into())
    } else {
        "unknown".into()
    };
    let socket_path = agent_dir.join("agent.sock").to_str().map(|s| s.to_string());
    let skills_count = count_skills(&home, name);
    let mcp_servers_count = count_mcp(&agent_dir);

    Ok(AgentStatusInfo {
        name: name.into(),
        running,
        pid,
        transport,
        socket_path,
        skills_count,
        mcp_servers_count,
    })
}

#[allow(dead_code)]
fn check_running(agent_dir: &std::path::Path) -> bool {
    let lock_path = agent_dir.join("running.lock");
    if lock_path.exists()
        && let Ok(bytes) = std::fs::read(&lock_path)
        && let Ok(lock) = serde_json::from_slice::<LockFile>(&bytes)
    {
        pid_alive(lock.pid)
    } else {
        false
    }
}

#[allow(dead_code)]
fn get_pid(agent_dir: &std::path::Path) -> Option<u32> {
    let lock_path = agent_dir.join("running.lock");
    if lock_path.exists()
        && let Ok(bytes) = std::fs::read(&lock_path)
        && let Ok(lock) = serde_json::from_slice::<LockFile>(&bytes)
    {
        Some(lock.pid)
    } else {
        None
    }
}

#[allow(dead_code)]
fn load_transport(profile_path: &std::path::Path) -> Result<String> {
    let yaml = std::fs::read_to_string(profile_path)?;
    let profile: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml)?;
    if profile.transport.socket.enabled {
        Ok("socket".into())
    } else if profile.transport.tcp.enabled {
        Ok("tcp".into())
    } else {
        Ok("stdio".into())
    }
}

#[allow(dead_code)]
fn count_skills(mur_home: &std::path::Path, agent_name: &str) -> usize {
    let skills_dir = mur_home.join("agents").join(agent_name).join("skills");
    if !skills_dir.exists() {
        return 0;
    }
    std::fs::read_dir(&skills_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .count()
        })
        .unwrap_or(0)
}

#[allow(dead_code)]
fn count_mcp(agent_dir: &std::path::Path) -> usize {
    let profile_path = agent_dir.join("profile.yaml");
    match std::fs::read_to_string(&profile_path) {
        Ok(yaml) => match serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml) {
            Ok(profile) => profile.mcp_servers.len(),
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

pub fn cmd_list(json: bool) -> Result<()> {
    let rows = collect_agents()?;
    if json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        let mur_home = resolve_mur_home()?;
        println!(
            "{:<20} {:<6} {:<10} {:<20} {:<10} {:<12}",
            "NAME", "EMOJI", "STATUS", "UPTIME", "PID", "CATEGORY"
        );
        for r in &rows {
            let pid = r
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string());
            let uptime = r.uptime.clone().unwrap_or_else(|| "-".to_string());
            // Detect stale-runtime for running agents. The STATUS column stays
            // plain "running" so fixed-width alignment is preserved; the marker
            // is appended as a trailing annotation after the last column.
            let stale_annotation = if r.status == "running" {
                let lock_path = mur_home.join("agents").join(&r.name).join("running.lock");
                let stale = mur_common::lock_file::read(&lock_path)
                    .ok()
                    .flatten()
                    .is_some_and(|lock| {
                        super::super::stale::is_stale(
                            &lock,
                            &super::super::stale::on_disk_sha_for(&r.name),
                        )
                    });
                if stale {
                    format!(" ⚠{STALE_SUFFIX}")
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            println!(
                "{:<20} {:<6} {:<10} {:<20} {:<10} {:<12}{}",
                r.name, r.emoji, r.status, uptime, pid, r.category, stale_annotation
            );
        }
    }
    Ok(())
}

pub fn cmd_status(name: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let rows = collect_agents()?;
    let Some(row) = rows.iter().find(|r| r.name == name) else {
        // A profile that does not parse keeps the agent out of the table, but
        // a runtime that refused on it left its reason behind (#2015).
        if let Some(lines) = status_refusal::unloadable_status(name, &mur_home.join("agents")) {
            for line in lines {
                println!("{line}");
            }
            return Ok(());
        }
        bail!("agent '{name}' not found");
    };
    println!("● {} - {}", row.name, row.category);
    println!(
        "   Loaded: {}",
        mur_home.join("agents").join(name).display()
    );
    // Check for stale runtime when the agent is running.
    let stale_suffix = if row.status == "running" {
        let on_disk = super::super::stale::on_disk_sha_for(name);
        let lock_path = mur_home.join("agents").join(name).join("running.lock");
        let stale = mur_common::lock_file::read(&lock_path)
            .ok()
            .flatten()
            .is_some_and(|lock| super::super::stale::is_stale(&lock, &on_disk));
        if stale { STALE_SUFFIX } else { "" }
    } else {
        ""
    };
    println!("   Active: {}{stale_suffix}", row.status);
    if let Some(r) =
        status_refusal::refusal_for(&mur_home.join("agents").join(name), row.status == "running")
    {
        for line in status_refusal::refusal_lines(name, &r) {
            println!("{line}");
        }
    }
    if super::super::service_migrate::needs_migration(name, &mur_home.join("agents").join(name)) {
        println!("{}", super::super::service_migrate::status_hint(name));
    }
    if let Some(pid) = row.pid {
        println!("     Main PID: {pid}");
    }
    if let Some(up) = &row.uptime {
        println!("       Uptime: {up}");
    }
    let agent_home = mur_home.join("agents").join(name);
    if let Some(line) = status_ssh_proxy::line_for(&agent_home) {
        println!("{line}");
    }
    for line in status_ssh_proxy::refusal_lines_for(&agent_home) {
        println!("{line}");
    }
    let lock_path = mur_home.join("agents").join(name).join("running.lock");
    for line in status_override::lines_for(&mur_home, name, &lock_path) {
        println!("{line}");
    }
    if let Some(line) = super::super::secret::keychain_pending_line(
        &mur_home.join("agents").join(name),
        name,
        // restart's own guard (`select_targets_with_on_disk`): lock file present.
        mur_home
            .join("agents")
            .join(name)
            .join("running.lock")
            .exists(),
    ) {
        println!("  {line}");
    }
    Ok(())
}

/// Why a directory under `agents/` is not a loadable agent, said in a way that
/// tells the reader what to do about it.
pub(super) fn describe_unloadable(dir: &std::path::Path) -> String {
    let legacy = dir.join("agent.yaml").exists();
    let has_key = dir.join("identity.key").exists();
    let what = match (legacy, has_key) {
        (true, true) => "has a legacy agent.yaml and still holds a signing key",
        (true, false) => "has a legacy agent.yaml",
        (false, true) => "has no profile.yaml but still holds a signing key",
        (false, false) => "has no profile.yaml",
    };
    // Naming the command is the difference between a warning and a remedy.
    // Without it the reader's obvious move is `rm -rf` on the directory — which
    // does not touch the launcher symlink or the service descriptor, so a
    // launchd job with `RunAtLoad` recreates the directory and the warning
    // comes back (field report, 2026-09-15). `mur agent remove` never loads the
    // profile, so it works on exactly these directories; the name is the
    // directory name, which is what it resolves by.
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let purge = if dir.join("identity.key").exists() {
        " — `--purge` also deletes the directory and destroys its signing key"
    } else {
        " — add `--purge` to delete the directory too"
    };
    format!(
        "is not a loadable agent: it {what} — not listed, not running, and not covered by anything that iterates agents. \
         `mur agent remove {name}` clears its launcher and service{purge}"
    )
}

fn collect_agents() -> Result<Vec<AgentRow>> {
    let mur_home = resolve_mur_home()?;
    let agents_dir = mur_home.join("agents");
    let mut rows = Vec::new();
    let entries = match fs::read_dir(&agents_dir) {
        Ok(e) => e,
        // An unreadable agents dir is not an empty fleet. Returning `Ok(rows)`
        // silently printed "no agents" to someone who has 29 of them.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(rows),
        Err(e) => {
            eprintln!(
                "warning: cannot read {} ({e}) — this listing is empty because \
                 the directory could not be read, not because there are no agents",
                agents_dir.display()
            );
            return Ok(rows);
        }
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // `agents/` is itself a git repo (the profile version store), so `.git`
        // lives here. Dot-directories are infrastructure, not stray agents —
        // warning about them every run would train the reader to ignore the
        // warning that matters.
        if dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
        let profile_path = dir.join("profile.yaml");
        if !profile_path.exists() {
            // The two arms below warn when a profile cannot be read or parsed;
            // a MISSING profile used to be the one skipped in silence. A
            // directory here without one is still an agent's worth of state —
            // typically a legacy `agent.yaml` layout, and often still holding a
            // signing key. Invisible in `mur agent list`, it is a key nobody
            // rotates and a directory nobody cleans up.
            eprintln!("warning: {} {}", dir.display(), describe_unloadable(&dir));
            continue;
        }
        let yaml = match fs::read_to_string(&profile_path) {
            Ok(y) => y,
            Err(e) => {
                eprintln!("warning: skipping {}: {e}", profile_path.display());
                continue;
            }
        };
        // A single malformed profile must not abort listing every other agent.
        // Mirror the pattern store's skip-and-warn behavior (store/yaml.rs).
        let profile: _AgentProfile = match serde_yaml_ng::from_str(&yaml) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("warning: skipping {}: {e}", profile_path.display());
                continue;
            }
        };

        let lock_path = dir.join("running.lock");
        let (status, emoji, pid, uptime) = classify(&lock_path);
        rows.push(AgentRow {
            name: profile.name,
            status,
            emoji,
            pid,
            uptime,
            category: format!("{:?}", profile.persona.category).to_lowercase(),
        });
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rows)
}

/// Classify an agent lock for the CLI table display.
///
/// Returns `(status_str, pid, uptime)`. Delegates to
/// `mur_common::lock_file::classify` for the 3-state liveness logic and
/// adds uptime computation (CLI-specific) on top.
fn classify(lock_path: &Path) -> (&'static str, &'static str, Option<u32>, Option<String>) {
    use mur_common::lock_file::AgentStatusKind;
    let status = mur_common::lock_file::classify(lock_path);
    let status_str = match status.kind {
        AgentStatusKind::Running => "running",
        AgentStatusKind::Stale => "stale",
        AgentStatusKind::Stopped => "stopped",
    };
    let emoji = status.kind.emoji();
    // Compute uptime from the lock file's started_at only when running.
    let uptime = if status.kind == AgentStatusKind::Running {
        mur_common::lock_file::read(lock_path)
            .ok()
            .flatten()
            .and_then(|lock| chrono::DateTime::parse_from_rfc3339(&lock.started_at).ok())
            .map(|start| {
                let secs = (chrono::Utc::now() - start.with_timezone(&chrono::Utc))
                    .num_seconds()
                    .max(0);
                format!("{}s", secs)
            })
    } else {
        None
    };
    (status_str, emoji, status.pid, uptime)
}
