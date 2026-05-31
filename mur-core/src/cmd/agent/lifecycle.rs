//! Agent lifecycle commands: create / list / status / stop / remove / rename.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use mur_common::agent::*;
use mur_common::identity::AgentIdentity;
use mur_common::{AgentProfile as _AgentProfile, LockFile};

use super::{
    pid_alive, refuse_if_running, resolve_bin_dir, resolve_mur_home, resolve_runtime_target,
    write_atomic,
};

pub fn cmd_create(
    name: &str,
    _no_interactive: bool,
    display_name: Option<String>,
    model: Option<String>,
    provider: Option<String>,
) -> Result<()> {
    validate_name(name)?;

    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if agent_home.exists() {
        bail!("agent {name} already exists at {}", agent_home.display());
    }
    fs::create_dir_all(&agent_home).with_context(|| format!("create {}", agent_home.display()))?;

    // P0a.5: generate Ed25519 identity keypair for cross-host A2A.
    let identity = AgentIdentity::generate();
    identity
        .save(&agent_home)
        .with_context(|| format!("save identity to {}", agent_home.display()))?;
    tracing::info!(
        pubkey = %identity.pubkey_text(),
        "generated identity keypair"
    );

    let display_name = display_name.unwrap_or_else(|| name.to_string());
    let raw_model = model.unwrap_or_else(|| "llama3.2:3b".to_string());
    // E2 fix: support `provider/model` shorthand in --model. Explicit --provider
    // wins. Default provider is `ollama` for backward compatibility.
    let (resolved_provider, resolved_model) = match provider {
        Some(p) => (p, raw_model),
        None => match raw_model.split_once('/') {
            Some((p, rest)) if !p.is_empty() && !rest.is_empty() && is_known_provider(p) => {
                (p.to_string(), rest.to_string())
            }
            _ => ("ollama".to_string(), raw_model),
        },
    };
    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::now_v7().to_string();

    // M1.3: Bootstrap rotation attestation — anchors the rotation chain
    // so future `mur agent rekey` calls have a verifiable starting point.
    use mur_common::identity::{RotationAttestation, RotationReason};
    let bootstrap_at = now.clone();
    let bootstrap = RotationAttestation::new(
        &id,
        "",
        identity.pubkey_text(),
        0,
        0,
        &bootstrap_at,
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    let bootstrap_line =
        serde_json::to_string(&bootstrap).context("serialize bootstrap attestation")?;
    let rotations_path = agent_home.join("rotations.jsonl");
    fs::write(&rotations_path, format!("{bootstrap_line}\n"))
        .with_context(|| format!("write {}", rotations_path.display()))?;
    tracing::debug!(uuid = %id, "bootstrap rotation attestation written");

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
            provider: resolved_provider,
            name: resolved_model,
            params: BTreeMap::new(),
        },
        model_ref: None,
        mcp_servers: vec![],
        skills: vec![],
        transport: TransportConfig {
            stdio: true,
            socket: SocketTransportConfig {
                enabled: true,
                bind: format!("unix://{}/agent.sock", agent_home.display()),
                auth: None,
            },
            tcp: TcpTransportConfig::default(),
            webhook: mur_common::agent::WebhookTransportConfig::default(),
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
            execution: ExecutionMode::default(),
            schedule: Vec::new(),
            idle_triggers: Vec::new(),
        },
        identity: IdentityConfig {
            pubkey: identity.pubkey_text(),
            owner: std::env::var("USER").ok(),
            created_at_key: Some(bootstrap_at.clone()),
            ..Default::default()
        },
        file_transfer: FileTransferConfig::default(),
        deployment: DeploymentConfig::default(),
        companion: CompanionConfig::default(),
        voice: mur_common::agent::VoiceConfig::default(),
        hooks: mur_common::HooksConfig::default(),
        trusted_peers: Vec::new(),
        appearance: mur_common::AgentAppearance::default(),
        federation: mur_common::FederationConfig::default(),
        file_actions: vec![],
        action_pipeline: mur_common::action::ActionPipelineConfig::default(),
        installed_skills: vec![],
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
    mur_common::validate_agent_name(name).with_context(|| format!("invalid agent name {name:?}"))
}

/// Whether a string is one of the providers we recognize when splitting
/// `provider/model` shorthand. Conservative allowlist to avoid mis-splitting
/// HuggingFace-style model ids like `meta-llama/Llama-3.2-3B`.
fn is_known_provider(s: &str) -> bool {
    matches!(
        s,
        "ollama"
            | "anthropic"
            | "openai"
            | "azure"
            | "bedrock"
            | "gemini"
            | "google"
            | "mistral"
            | "together"
            | "groq"
            | "fireworks"
            | "deepseek"
            | "xai"
            | "cohere"
    )
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
        llm: Default::default(),
    }
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

/// Classify an agent lock for the CLI table display.
///
/// Returns `(status_str, pid, uptime)`. Delegates to
/// `mur_common::lock_file::classify` for the 3-state liveness logic and
/// adds uptime computation (CLI-specific) on top.
fn classify(lock_path: &Path) -> (&'static str, Option<u32>, Option<String>) {
    use mur_common::lock_file::AgentStatusKind;
    let status = mur_common::lock_file::classify(lock_path);
    let status_str = match status.kind {
        AgentStatusKind::Running => "running",
        AgentStatusKind::Stale => "stale",
        AgentStatusKind::Stopped => "stopped",
    };
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
    (status_str, status.pid, uptime)
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

pub fn cmd_remove(name: &str, purge: bool, force: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    if !force {
        let unread = count_unread_companion_inbox(&agent_home);
        if unread > 0 {
            bail!(
                "agent '{name}' has {unread} unread companion message{}. Run 'mur agent companion inbox {name} --unread-only' to view, or pass --force to remove anyway",
                if unread == 1 { "" } else { "s" }
            );
        }
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

/// Count files in `<agent_home>/companion/inbox/*.md` whose response line is `<unset>`.
/// Returns 0 if the directory does not exist (companion never enabled).
fn count_unread_companion_inbox(agent_home: &Path) -> usize {
    let inbox = agent_home.join("companion/inbox");
    if !inbox.exists() {
        return 0;
    }
    let entries = match std::fs::read_dir(&inbox) {
        Ok(it) => it,
        Err(_) => return 0,
    };
    let mut n = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if let Ok(body) = std::fs::read_to_string(&path)
            && is_unread(&body)
        {
            n += 1;
        }
    }
    n
}

/// Returns `true` when the last `>>> response: ` line in `body` has the value `<unset>`.
fn is_unread(body: &str) -> bool {
    let marker = ">>> response: ";
    body.lines()
        .rev()
        .find_map(|l| l.strip_prefix(marker).map(|v| v.trim() == "<unset>"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests_remove_unread_guard {
    use super::*;
    use tempfile::TempDir;

    fn write_inbox_unread(dir: &Path, id: &str) {
        let inbox = dir.join("companion/inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let body = format!(
            "---\nid: {id}\nsituation: morning_greeting\ntemplate_id: t\nlocale: en-US\ngenerated_at: 2026-04-29T08:00:00+00:00\n---\n\nHello!\n\n>>> response: <unset>\n"
        );
        std::fs::write(inbox.join(format!("{id}.md")), body).unwrap();
    }

    fn write_inbox_acked(dir: &Path, id: &str) {
        let inbox = dir.join("companion/inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let body = format!(
            "---\nid: {id}\nsituation: morning_greeting\ntemplate_id: t\nlocale: en-US\ngenerated_at: 2026-04-29T08:00:00+00:00\n---\n\nHello!\n\n>>> response: good\n"
        );
        std::fs::write(inbox.join(format!("{id}.md")), body).unwrap();
    }

    #[test]
    fn count_unread_returns_zero_for_no_inbox() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(count_unread_companion_inbox(tmp.path()), 0);
    }

    #[test]
    fn count_unread_counts_only_unset_response() {
        let tmp = TempDir::new().unwrap();
        write_inbox_unread(tmp.path(), "msg-001");
        write_inbox_unread(tmp.path(), "msg-002");
        write_inbox_acked(tmp.path(), "msg-003");
        assert_eq!(count_unread_companion_inbox(tmp.path()), 2);
    }
}
