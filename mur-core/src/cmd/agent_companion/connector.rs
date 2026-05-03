//! `mur agent companion connector ...` — bridge agent scaffolding (Track C1).
//!
//! In Track C1 only `--platform stub` is supported. The stub is a fully
//! functional A2A bridge agent (LLM disabled, identity keypair, default
//! route) that downstream tracks (C2 Telegram, C3 send-from-any-app)
//! specialise.

use anyhow::{Context, Result, bail};

/// Scaffold a new bridge agent. Currently only `--platform stub` is supported.
pub async fn add(name: String, platform: &str, default_route: &str) -> Result<()> {
    if platform != "stub" {
        bail!(
            "platform '{platform}' not supported in Track C1 — only 'stub' is available. \
             Telegram lands in C2; send-from-any-app in C3."
        );
    }
    if default_route.trim().is_empty() {
        bail!("--default-route must be non-empty");
    }
    scaffold_stub_bridge(&name, default_route).await
}

/// Build a fresh stub-bridge agent directory under `$MUR_HOME/agents/<name>/`
/// containing `profile.yaml`, `routes.yaml`, `identity.{key,pub}`, and a
/// placeholder `sys_prompt.md`. The profile is constructed via direct struct
/// instantiation (instead of a fixture-yaml round-trip) so future schema
/// fields with `#[serde(default)]` don't drift the scaffolded output.
pub(crate) async fn scaffold_stub_bridge(name: &str, default_route: &str) -> Result<()> {
    use mur_common::agent::{
        BackoffStrategy, CommunicationConfig, CompanionConfig, DeploymentConfig, Entitlements,
        ExecutionMode, FileTransferConfig, FilesystemEntitlement, IdentityConfig, InboundNetwork,
        LifecycleConfig, LimitsEntitlement, ModelConfig, NetworkEntitlement, NetworkOutboundMode,
        NotificationsConfig, OutboundNetwork, Persona, PersonaCategory, PersonaTraits,
        ProcessesEntitlement, ResolveDnsConfig, RestartPolicy, RetryConfig, RetryPolicy,
        SocketTransportConfig, SpawnEntitlement, SpawnMode, SyscallsEntitlement,
        TcpTransportConfig, TransportConfig,
    };
    use mur_common::bridge::routes::BridgeRouteConfig;
    use mur_common::identity::AgentIdentity;
    use mur_common::{AgentProfile, LlmEntitlement, LlmMode};
    use std::path::PathBuf;

    mur_common::validate_agent_name(name)
        .with_context(|| format!("invalid bridge agent name {name:?}"))?;

    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .expect("home dir resolvable")
                .join(".mur")
        });
    let dir = mur_home.join("agents").join(name);
    if dir.exists() {
        bail!("agent dir already exists: {}", dir.display());
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    // 1. identity keypair (Ed25519)
    let id = AgentIdentity::generate();
    id.save(&dir)
        .with_context(|| format!("save identity to {}", dir.display()))?;
    let pubkey = id.pubkey_text();

    // 2. routes.yaml (bridge-specific schema)
    let routes = BridgeRouteConfig {
        default_route: default_route.to_string(),
        routes: vec![],
    };
    std::fs::write(
        dir.join("routes.yaml"),
        serde_yaml_ng::to_string(&routes).context("serialize routes.yaml")?,
    )
    .with_context(|| format!("write {}/routes.yaml", dir.display()))?;

    // 3. profile.yaml — typed AgentProfile, not string-formatted, so future
    //    schema additions don't drift. LLM is `off` so the supervisor refuses
    //    to construct an LLM client; outbound network is `off` because a
    //    stub bridge has no upstream to call.
    let now = chrono::Utc::now().to_rfc3339();
    let profile_id = uuid::Uuid::now_v7().to_string();
    let profile = AgentProfile {
        schema: 1,
        id: profile_id,
        name: name.to_string(),
        display_name: name.to_string(),
        version: "0.1.0".to_string(),
        persona: Persona {
            category: PersonaCategory::Custom,
            description: format!("Bridge agent {name} (LLM disabled)"),
            traits: PersonaTraits {
                tone: "concise".into(),
                risk: "cautious".into(),
                verbosity: "low".into(),
            },
        },
        sys_prompt_file: "sys_prompt.md".into(),
        // model is required by the schema even though llm.mode = off blocks
        // its use; supervisor never instantiates a client.
        model: ModelConfig {
            provider: "none".into(),
            name: "none".into(),
            params: std::collections::BTreeMap::new(),
        },
        model_ref: None,
        mcp_servers: vec![],
        skills: vec![],
        transport: TransportConfig {
            stdio: true,
            socket: SocketTransportConfig {
                enabled: true,
                bind: format!("unix://{}/agent.sock", dir.display()),
                auth: None,
            },
            tcp: TcpTransportConfig::default(),
        },
        communication: CommunicationConfig {
            accepts_from: vec!["*".into()],
            sends_to: vec![],
        },
        capabilities: vec!["a2a.message.send".into(), "a2a.tasks".into()],
        entitlements: Entitlements {
            network: NetworkEntitlement {
                inbound: InboundNetwork { ports: vec![] },
                outbound: OutboundNetwork {
                    mode: NetworkOutboundMode::Off,
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
                memory_mb: 256,
                file_descriptors: 512,
                processes: 16,
            },
            llm: LlmEntitlement { mode: LlmMode::Off },
        },
        notifications: NotificationsConfig::default(),
        retry: RetryConfig {
            llm: RetryPolicy {
                max_retries: 0,
                backoff: BackoffStrategy::Fixed,
                initial_delay_ms: 0,
                max_delay_ms: None,
                retry_on: vec![],
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
        },
        identity: IdentityConfig {
            pubkey: pubkey.clone(),
            owner: std::env::var("USER").ok(),
            algorithm: "ed25519".into(),
            key_version: 0,
            created_at_key: Some(now.clone()),
            ..Default::default()
        },
        file_transfer: FileTransferConfig::default(),
        deployment: DeploymentConfig::default(),
        companion: CompanionConfig::default(),
        trusted_peers: vec![],
        created_at: now.clone(),
        updated_at: now,
    };
    std::fs::write(
        dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).context("serialize profile.yaml")?,
    )
    .with_context(|| format!("write {}/profile.yaml", dir.display()))?;

    // 4. sys_prompt placeholder — schema requires the file to exist, but the
    //    supervisor never reads it because llm.mode = off.
    std::fs::write(
        dir.join("sys_prompt.md"),
        "# Bridge sys_prompt\nThis agent is a bridge (llm.mode = off).\n",
    )
    .with_context(|| format!("write {}/sys_prompt.md", dir.display()))?;

    println!("stub bridge '{name}' scaffolded at {}", dir.display());
    println!("   pubkey: {pubkey}");
    println!("   default_route: {default_route}");
    println!("   trusted_peers: []  (user agent must add this bridge to its trusted_peers[])");
    Ok(())
}
