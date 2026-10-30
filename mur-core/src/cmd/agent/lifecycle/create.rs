use super::*;

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
        smart: None,
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
        secrets: Vec::new(),
        addons: Vec::new(),
        hitl: mur_common::HitlConfig::default(),
        created_at: now.clone(),
        updated_at: now,
        requires_programs: Vec::new(),
        requires_capabilities: Vec::new(),
        limits: None,
        extra: Default::default(),
    };

    let yaml = serde_yaml_ng::to_string(&profile).context("serialize profile.yaml")?;
    write_atomic(&agent_home.join("profile.yaml"), yaml.as_bytes())?;
    // #712: a fresh profile is trusted by construction; pin it now so a stale
    // pin from an earlier agent of this name cannot block the first start.
    let _ = crate::store::entitlements_pin::advance_entitlements_pin(
        &agent_home.join("profile.yaml"),
        None,
        &profile.entitlements,
    );
    write_atomic(
        &agent_home.join("sys_prompt.md"),
        default_sys_prompt(name).as_bytes(),
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
pub(super) fn near_registry_key(mur_home: &Path, typed: &str) -> Result<Option<String>> {
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
pub(super) fn resolve_model_ref_for_create(
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

/// The placeholder system prompt `mur agent create` writes. Named so a caller
/// that installs a role prompt can tell "never edited" from "user-written".
pub(crate) fn default_sys_prompt(name: &str) -> String {
    format!("# {name}\n\nYou are an assistant.\n")
}

pub(super) fn validate_name(name: &str) -> Result<()> {
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
                allow_ports: vec![],
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
        ssh: Default::default(),
    }
}
