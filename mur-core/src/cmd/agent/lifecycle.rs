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
    //
    // Bug fix: a bare `--model` value (no `--provider`, no `provider/model`
    // slash form) that exactly matches an existing `models.yaml` registry key
    // binds the agent to that entry directly, instead of silently falling
    // through to the ollama default and leaving `model_ref` unset (StubEcho).
    let alias_entry = if provider.is_none() {
        registry_alias_entry(&mur_home, &raw_model)?
    } else {
        None
    };
    let (resolved_provider, resolved_model, resolved_model_ref) =
        if let Some((entry_provider, entry_model)) = alias_entry {
            (entry_provider, entry_model, Some(raw_model.clone()))
        } else {
            let (resolved_provider, resolved_model) = match provider {
                Some(p) => (p, raw_model),
                None => match raw_model.split_once('/') {
                    Some((p, rest))
                        if !p.is_empty() && !rest.is_empty() && is_known_provider(p) =>
                    {
                        (p.to_string(), rest.to_string())
                    }
                    _ => ("ollama".to_string(), raw_model),
                },
            };
            // Bind the agent to a models.yaml registry entry so the runtime can
            // resolve real credentials + base_url via `model_ref` (the path working
            // agents use). Writing only the inline `model:` block leaves the runtime
            // with no secret for cloud providers, which silently degrades to StubEcho.
            // Local backends (ollama/local) need no secret, so we leave them inline.
            let model_ref = resolve_model_ref_for_create(
                &mur_home,
                Some(resolved_provider.as_str()),
                &resolved_model,
            )?;
            (resolved_provider, resolved_model, model_ref)
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
        role: None,
        effort: None,
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
        model_ref: resolved_model_ref,
        fallback_chain: Vec::new(),
        routing: None,
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
        disabled_skills: Vec::new(),
        disabled_mcp: Vec::new(),
        addons: Vec::new(),
        hitl: mur_common::HitlConfig::default(),
        created_at: now.clone(),
        updated_at: now,
        requires_programs: Vec::new(),
        requires_capabilities: Vec::new(),
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
    // Prefer the canonical runtime that lives in this same bin dir — the copy
    // `build.sh --install` / `mur update` keep fresh. `resolve_runtime_target()`
    // points at whatever sits beside the *current* `mur`, which for a Homebrew
    // install is inside the keg: an absolute symlink into a directory no
    // installer ever refreshes, so the agent silently never upgrades again.
    let canonical = bin_dir.join(if cfg!(windows) {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    });
    let target = if canonical.exists() {
        canonical
    } else {
        resolve_runtime_target()
    };
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &symlink)
        .with_context(|| format!("symlink {} -> {}", symlink.display(), target.display()))?;
    #[cfg(windows)]
    std::fs::copy(&target, &symlink)
        .with_context(|| format!("copy {} to {}", target.display(), symlink.display()))?;

    println!("Created agent '{}' at {}", name, agent_home.display());
    // Say what it bound to. `--model` accepts a registry key, a bare model id
    // and `provider/model`, and a value that misses a registry key by one
    // character silently becomes an ollama binding for a model ollama does not
    // have — an agent that looks created and answers nothing.
    match &profile.model_ref {
        Some(r) => println!(
            "Model: {}/{} via registry key '{r}'",
            profile.model.provider, profile.model.name
        ),
        None => {
            println!(
                "Model: {}/{} (no registry key — no stored credential)",
                profile.model.provider, profile.model.name
            );
            if let Some(near) = near_registry_key(&mur_home, &profile.model.name)? {
                println!("       did you mean the registry key '{near}'? (`mur model list`)");
            }
        }
    }
    println!("Symlink: {} -> {}", symlink.display(), target.display());
    Ok(())
}

/// A registry key that differs from `typed` only in punctuation or case —
/// `anthropic_claude_opus5` for `anthropic_claude_opus_5`. Returns `Some` only
/// when exactly one key matches, so the hint is never a guess between two.
fn near_registry_key(mur_home: &Path, typed: &str) -> Result<Option<String>> {
    use mur_common::model::ModelRegistry;

    let squash = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    let target = squash(typed);
    if target.is_empty() {
        return Ok(None);
    }
    let reg_path = mur_home.join("models.yaml");
    let Ok(reg) = ModelRegistry::load_from(&reg_path) else {
        return Ok(None);
    };
    let mut hits = reg.models.keys().filter(|k| squash(k) == target);
    let first = hits.next().cloned();
    Ok(match hits.next() {
        Some(_) => None,
        None => first,
    })
}

/// Look up an exact registry key match for a bare `--model` value (e.g.
/// `claude_sonnet`). Returns the entry's `(provider, model)` when found.
fn registry_alias_entry(mur_home: &Path, key: &str) -> Result<Option<(String, String)>> {
    use mur_common::model::ModelRegistry;

    let reg_path = mur_home.join("models.yaml");
    let reg = ModelRegistry::load_from(&reg_path)
        .with_context(|| format!("load model registry {}", reg_path.display()))?;
    Ok(reg
        .models
        .get(key)
        .map(|entry| (entry.provider.clone(), entry.model.clone())))
}

/// Resolve the `model_ref` an agent should carry so the runtime loads real
/// credentials from `~/.mur/models.yaml`. Cloud providers (anthropic, openai,
/// …) require a registry entry to supply the secret + proxy base URL; without
/// one the runtime falls back to StubEcho. Local backends (`ollama`, `local`)
/// authenticate without a secret, so they stay on the inline `model:` block.
///
/// Preference order for cloud providers:
///   1. Reuse an existing registry entry whose provider+model match (inherits
///      its secret + base_url — this is how the working agents are wired).
///   2. Otherwise upsert a new secretless entry keyed by provider+model so the
///      binding at least exists and the user has a single place to add a secret.
fn resolve_model_ref_for_create(
    mur_home: &Path,
    provider: Option<&str>,
    model: &str,
) -> Result<Option<String>> {
    use mur_common::model::{ModelEntry, ModelRegistry};

    // Bare model, no explicit provider: if it's an exact registry key, bind
    // to that entry directly rather than falling through to the ollama
    // default (which would silently leave model_ref unset / StubEcho).
    if provider.is_none() && registry_alias_entry(mur_home, model)?.is_some() {
        return Ok(Some(model.to_string()));
    }

    let provider = provider.unwrap_or("ollama");

    // Local backends do not need a registry-supplied secret.
    if matches!(provider, "ollama" | "local") {
        return Ok(None);
    }

    let reg_path = mur_home.join("models.yaml");
    let mut reg = ModelRegistry::load_from(&reg_path)
        .with_context(|| format!("load model registry {}", reg_path.display()))?;

    // 1. Reuse an existing matching entry (case-insensitive provider match,
    //    exact model match) — inherits its secret + base_url.
    if let Some((key, _)) = reg
        .models
        .iter()
        .find(|(_, e)| e.provider.eq_ignore_ascii_case(provider) && e.model == model)
    {
        return Ok(Some(key.clone()));
    }

    // 2. No match: upsert a new (secretless) entry so the binding exists.
    let key = sanitize_ref_name(&format!("{provider}_{model}"));
    reg.models.entry(key.clone()).or_insert_with(|| ModelEntry {
        provider: provider.to_string(),
        model: model.to_string(),
        base_url: None,
        secret: None,
        capabilities: vec![],
        params: serde_json::Value::Null,
        tier: None,
        cost_per_1k_tokens: None,
        ..Default::default()
    });
    reg.save_to(&reg_path)
        .with_context(|| format!("write model registry {}", reg_path.display()))?;
    Ok(Some(key))
}

/// Sanitize a provider/model pair into a stable, filesystem/YAML-safe registry
/// key (mirrors `model_resolve::choice_ref_name`).
fn sanitize_ref_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
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
                allowed_dirs: vec![],
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
        tools: vec![],
        fail_closed_on_sandbox_error: true,
    }
}

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
                        super::stale::is_stale(&lock, &super::stale::on_disk_sha_for(&r.name))
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
    let row = rows
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| anyhow!("agent '{name}' not found"))?;
    println!("● {} - {}", row.name, row.category);
    println!(
        "   Loaded: {}",
        mur_home.join("agents").join(name).display()
    );
    // Check for stale runtime when the agent is running.
    let stale_suffix = if row.status == "running" {
        let on_disk = super::stale::on_disk_sha_for(name);
        let lock_path = mur_home.join("agents").join(name).join("running.lock");
        let stale = mur_common::lock_file::read(&lock_path)
            .ok()
            .flatten()
            .is_some_and(|lock| super::stale::is_stale(&lock, &on_disk));
        if stale { STALE_SUFFIX } else { "" }
    } else {
        ""
    };
    println!("   Active: {}{stale_suffix}", row.status);
    if let Some(pid) = row.pid {
        println!("     Main PID: {pid}");
    }
    if let Some(up) = &row.uptime {
        println!("       Uptime: {up}");
    }
    Ok(())
}

/// Why a directory under `agents/` is not a loadable agent, said in a way that
/// tells the reader what to do about it.
fn describe_unloadable(dir: &std::path::Path) -> String {
    let legacy = dir.join("agent.yaml").exists();
    let has_key = dir.join("identity.key").exists();
    let what = match (legacy, has_key) {
        (true, true) => "has a legacy agent.yaml and still holds a signing key",
        (true, false) => "has a legacy agent.yaml",
        (false, true) => "has no profile.yaml but still holds a signing key",
        (false, false) => "has no profile.yaml",
    };
    format!(
        "is not a loadable agent: it {what} — not listed, not running, and not covered by anything that iterates agents"
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

    // Take the agent out of its supervisor's hands before signalling it.
    // launchd/systemd restart the runtime the instant it dies (every service
    // `mur agent install-service` writes sets KeepAlive), so signalling alone
    // cannot stop a service-managed agent — it only swaps the pid.
    let had_service = super::service::stop_service(name);

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

    // Confirm, don't assert. Signalling the pid is not the same as the agent
    // being down: a launchd job with KeepAlive (every service `mur agent
    // install-service` writes) hands back a fresh pid within a second, and the
    // old code printed "Stopped agent" regardless — so the arrow read
    // 17402 → 17597 while the message said success.
    //
    // `stop_service` above removes that race for our own services. This loop
    // catches whatever else might be respawning it, and says so instead of
    // lying.
    if let Some(pid) = wait_for_no_live_lock(&lock_path) {
        bail!(
            "signalled '{name}' but it is running again as pid {pid} — something is respawning it.{}",
            if had_service {
                ""
            } else {
                " No mur service is installed for it; check for another supervisor."
            }
        );
    }
    if had_service {
        // "unloaded its service" read as permanent, and it is not: the
        // descriptor stays on disk with RunAtLoad and is not disabled, so the
        // next login starts the agent again. Say that here, where the user
        // forms the belief, instead of letting them discover it after a
        // reboot.
        println!("Stopped agent '{name}' and unloaded its service for this session");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        println!(
            "  It starts again at your next login. To keep it stopped: {}",
            super::service::lasting_stop_hint(name)
        );
    } else {
        println!("Stopped agent '{name}'");
    }
    Ok(())
}

/// Poll briefly for a *live* `running.lock` to reappear, returning the pid that
/// holds it. A supervisor that restarts the runtime needs a moment to do it, so
/// checking once immediately after the kill would always look clean.
fn wait_for_no_live_lock(lock_path: &Path) -> Option<u32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut seen = None;
    while std::time::Instant::now() < deadline {
        seen = fs::read(lock_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<LockFile>(&b).ok())
            .filter(|l| pid_alive(l.pid))
            .map(|l| l.pid);
        if seen.is_some() {
            return seen;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    seen
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

    // The symlink above is what the service execs, so leaving the descriptor
    // behind leaves the supervisor retrying a path that no longer exists — and
    // loading it again at the next login. Remove goes with it.
    if let Some(path) = super::service::remove_service(name) {
        println!("Removed service {}", path.display());
    }

    if purge {
        if let Some(key_dir) = purge_agent_state(&agent_home)? {
            println!("Removed private key {}", key_dir.display());
        }
        println!("Purged agent '{name}'");
    } else {
        println!(
            "Removed agent '{name}' (data preserved at {})",
            agent_home.display()
        );
    }
    Ok(())
}

/// Remove everything `--purge` promises: the agent home **and** the private key
/// that has lived outside it since #850 option (c) moved keys to
/// `<mur_home>/keys/<name>/`. Returns the key directory removed, if any, so the
/// caller can report it.
///
/// Removing only the agent home is what this fixes: `--purge` says it deletes
/// the agent's data, but a 32-byte Ed25519 private key kept outliving its
/// agent. Those orphans are invisible — nothing enumerates them — and they
/// accumulate silently, one per purge.
///
/// The key goes FIRST on purpose. If it fails the agent home is still intact
/// and the command can be retried; the reverse order fails into exactly the
/// state being fixed — a key with no agent to own it.
fn purge_agent_state(agent_home: &Path) -> Result<Option<PathBuf>> {
    // `private_key_dir` returns its argument unchanged for directories that are
    // not an agent home (commander, publisher, the host key). Comparing guards
    // against deleting the agent home twice in that case.
    let key_dir = mur_common::identity::private_key_dir(agent_home);
    let removed = if key_dir != agent_home && key_dir.is_dir() {
        fs::remove_dir_all(&key_dir)
            .with_context(|| format!("remove_dir_all {}", key_dir.display()))?;
        Some(key_dir)
    } else {
        None
    };
    fs::remove_dir_all(agent_home)
        .with_context(|| format!("remove_dir_all {}", agent_home.display()))?;
    Ok(removed)
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
mod tests_resolve_model_ref_for_create {
    use super::*;
    use mur_common::model::{ModelEntry, ModelRegistry};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn seed_models_yaml(home: &TempDir, key: &str, provider: &str, model: &str) {
        let mut models = BTreeMap::new();
        models.insert(
            key.to_string(),
            ModelEntry {
                provider: provider.to_string(),
                model: model.to_string(),
                ..Default::default()
            },
        );
        let reg = ModelRegistry {
            schema_version: 1,
            models,
            roles: BTreeMap::new(),
        };
        reg.save_to(&home.path().join("models.yaml")).unwrap();
    }

    #[test]
    fn create_with_bare_alias_sets_model_ref() {
        let home = TempDir::new().unwrap();
        seed_models_yaml(&home, "claude_sonnet", "anthropic", "claude-sonnet-5");

        let mr = resolve_model_ref_for_create(home.path(), None, "claude_sonnet").unwrap();

        assert_eq!(mr, Some("claude_sonnet".to_string()));
    }

    #[test]
    fn create_with_unknown_bare_model_leaves_model_ref_unset() {
        let home = TempDir::new().unwrap();
        seed_models_yaml(&home, "claude_sonnet", "anthropic", "claude-sonnet-5");

        // Not a registry key, and provider is None -> caller defaults to
        // ollama before ever calling this function, so this case should not
        // be reached with a non-alias bare model; verify no false alias hit.
        let mr = resolve_model_ref_for_create(home.path(), None, "llama3.2:3b").unwrap();

        assert_eq!(mr, None);
    }

    #[test]
    fn explicit_provider_still_matches_registry_entry() {
        let home = TempDir::new().unwrap();
        seed_models_yaml(&home, "claude_sonnet", "anthropic", "claude-sonnet-5");

        let mr = resolve_model_ref_for_create(home.path(), Some("anthropic"), "claude-sonnet-5")
            .unwrap();

        assert_eq!(mr, Some("claude_sonnet".to_string()));
    }
}

#[cfg(test)]
mod tests_near_registry_key {
    use super::*;
    use mur_common::model::{ModelEntry, ModelRegistry};

    fn seed(home: &std::path::Path, keys: &[&str]) {
        let mut reg = ModelRegistry::default();
        for k in keys {
            reg.models.insert((*k).into(), ModelEntry::default());
        }
        reg.save_to(&home.join("models.yaml")).unwrap();
    }

    /// A one-character miss on a registry key used to produce an ollama agent
    /// bound to a model ollama does not have, reported as a success.
    #[test]
    fn names_the_key_only_when_it_is_unambiguous() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        seed(home, &["anthropic_claude_opus_5", "omlx"]);

        // Missing underscore, wrong case, no separators — all the same key.
        for typed in [
            "anthropic_claude_opus5",
            "Anthropic-Claude-Opus-5",
            "anthropicclaudeopus5",
        ] {
            assert_eq!(
                near_registry_key(home, typed).unwrap().as_deref(),
                Some("anthropic_claude_opus_5"),
                "typed {typed}"
            );
        }
        // A genuine ollama model id must not be dressed up as a typo.
        assert_eq!(near_registry_key(home, "llama3.2:3b").unwrap(), None);
        assert_eq!(near_registry_key(home, "").unwrap(), None);
    }

    #[test]
    fn stays_silent_when_two_keys_collide() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        // Both reduce to "gpt5".
        seed(home, &["gpt_5", "gpt-5"]);
        assert_eq!(
            near_registry_key(home, "gpt5").unwrap(),
            None,
            "a hint that has to pick between two keys is a guess"
        );
    }

    #[test]
    fn missing_registry_is_not_an_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(near_registry_key(tmp.path(), "anything").unwrap(), None);
    }
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

#[cfg(test)]
mod tests_purge_removes_private_key {
    use super::*;
    use tempfile::TempDir;

    /// The regression: `--purge` deleted the agent home and left
    /// `<mur_home>/keys/<name>/identity.key` behind. Nothing enumerates that
    /// tree, so the orphans were only found by counting `keys/` against
    /// `agents/` by hand.
    #[test]
    fn purge_removes_the_key_that_lives_outside_the_agent_home() {
        let tmp = TempDir::new().unwrap();
        let agent_home = tmp.path().join("agents").join("gone");
        let key_dir = tmp.path().join("keys").join("gone");
        std::fs::create_dir_all(&agent_home).unwrap();
        std::fs::create_dir_all(&key_dir).unwrap();
        std::fs::write(key_dir.join("identity.key"), b"not-a-real-key").unwrap();

        let removed = purge_agent_state(&agent_home).unwrap();

        assert_eq!(removed.as_deref(), Some(key_dir.as_path()));
        assert!(!agent_home.exists(), "agent home survived the purge");
        assert!(!key_dir.exists(), "private key outlived its agent");
    }

    /// An agent whose key was never created must still purge cleanly rather
    /// than failing on a missing directory.
    #[test]
    fn purge_succeeds_when_the_agent_never_had_a_key() {
        let tmp = TempDir::new().unwrap();
        let agent_home = tmp.path().join("agents").join("keyless");
        std::fs::create_dir_all(&agent_home).unwrap();

        assert!(purge_agent_state(&agent_home).unwrap().is_none());
        assert!(!agent_home.exists());
    }

    /// A directory that is not under `agents/` maps to itself, and must not be
    /// removed twice — `private_key_dir` deliberately passes those through.
    #[test]
    fn purge_does_not_double_remove_a_non_agent_directory() {
        let tmp = TempDir::new().unwrap();
        let stray = tmp.path().join("commander");
        std::fs::create_dir_all(&stray).unwrap();

        assert!(purge_agent_state(&stray).unwrap().is_none());
        assert!(!stray.exists());
    }
}

#[cfg(test)]
mod tests_unloadable_agent_dirs {
    use super::describe_unloadable;

    /// The message has to say which of the four situations it is, because the
    /// remedy differs: a legacy layout wants migrating, a stray directory
    /// wants deleting, and either one still holding a signing key wants that
    /// key dealt with first.
    #[test]
    fn the_message_names_a_signing_key_and_a_legacy_layout() {
        let tmp = tempfile::tempdir().unwrap();

        let legacy_with_key = tmp.path().join("Author");
        std::fs::create_dir_all(&legacy_with_key).unwrap();
        std::fs::write(legacy_with_key.join("agent.yaml"), b"name: Author").unwrap();
        std::fs::write(legacy_with_key.join("identity.key"), b"k").unwrap();
        let m = describe_unloadable(&legacy_with_key);
        assert!(m.contains("legacy agent.yaml"), "{m}");
        assert!(m.contains("signing key"), "{m}");

        let stray_with_key = tmp.path().join("orphan");
        std::fs::create_dir_all(&stray_with_key).unwrap();
        std::fs::write(stray_with_key.join("identity.key"), b"k").unwrap();
        let m = describe_unloadable(&stray_with_key);
        assert!(m.contains("signing key"), "{m}");
        assert!(!m.contains("legacy"), "{m}");

        let empty = tmp.path().join("wztest");
        std::fs::create_dir_all(empty.join("outbox")).unwrap();
        let m = describe_unloadable(&empty);
        assert!(m.contains("no profile.yaml"), "{m}");
        assert!(!m.contains("signing key"), "{m}");
    }
}
