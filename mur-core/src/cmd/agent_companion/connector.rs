//! `mur agent companion connector ...` — bridge agent scaffolding.
//!
//! Track C1 introduced `--platform stub` (a fully functional A2A bridge with
//! LLM disabled). Track C2 wires `--platform telegram`: the BotFather 5-step
//! setup UX (M-c2.1) plus a non-interactive flag path used by the integration
//! tests and CI.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::Duration;

use mur_common::bridge::{PrivacyMode, SlackConfig, SlackPrivacyMode, TelegramConfig};

use crate::bridge_keychain::{Keychain, MockKeychain, SystemKeychain};

/// Scaffold a new bridge agent.
///
/// Track C1 supports `--platform stub`. Track C2 wires `--platform telegram`
/// with both the interactive 5-step BotFather flow (DEFAULT, when no
/// `--bot-token` is given) and a non-interactive flag path used by tests and
/// CI: `--bot-token`, `--bot-username`, `--chat-id`, `--ack`.
#[allow(clippy::too_many_arguments)]
pub async fn add(
    name: String,
    platform: &str,
    default_route: &str,
    bot_token: Option<String>,
    bot_username: Option<String>,
    chat_id: Option<i64>,
    ack: bool,
    allow_groups: Vec<i64>,
) -> Result<()> {
    if default_route.trim().is_empty() {
        bail!("--default-route must be non-empty");
    }
    match platform {
        "stub" => scaffold_stub_bridge(&name, default_route).await,
        "telegram" => {
            // First scaffold the underlying stub bridge directory (identity,
            // profile.yaml, routes.yaml, sys_prompt.md). The Telegram-specific
            // `telegram.yaml` is then written alongside.
            scaffold_stub_bridge(&name, default_route).await?;
            run_telegram_setup(&name, bot_token, bot_username, chat_id, ack, allow_groups).await
        }
        "slack" => {
            scaffold_stub_bridge(&name, default_route).await?;
            run_slack_setup(&name).await
        }
        other => bail!(
            "platform '{other}' not supported — recognised: 'stub', 'telegram', 'slack'. \
             Send-from-any-app lands in C3."
        ),
    }
}

/// Telegram setup driver. Picks between the non-interactive flag path
/// (full `--bot-token` + `--bot-username` + `--chat-id` + `--ack`) and the
/// interactive 5-step BotFather flow.
async fn run_telegram_setup(
    bridge_id: &str,
    bot_token: Option<String>,
    bot_username: Option<String>,
    chat_id: Option<i64>,
    ack: bool,
    allow_groups: Vec<i64>,
) -> Result<()> {
    let kc: Box<dyn Keychain> = if std::env::var("MUR_TELEGRAM_KEYCHAIN_BACKEND")
        .ok()
        .as_deref()
        == Some("mock")
    {
        Box::new(MockKeychain::default())
    } else {
        Box::new(SystemKeychain)
    };

    // Non-interactive path — all flags supplied.
    if let (Some(token), Some(username), Some(cid)) =
        (bot_token.as_deref(), bot_username.as_deref(), chat_id)
    {
        let args = ScaffoldArgs {
            bridge_id: bridge_id.into(),
            bot_token: token.into(),
            bot_username: username.into(),
            chat_id: cid,
            ack,
            allow_groups,
        };
        let outcome = scaffold_telegram_bridge(args, kc.as_ref())?;
        let ScaffoldOutcome::Ok { profile_path, .. } = outcome;
        println!(
            "telegram bridge scaffolded; config: {}",
            profile_path.display()
        );
        return Ok(());
    }

    // Interactive path — drive BotFather 5-step UX. We only run this when at
    // least one of the non-interactive flags is missing. On a non-tty test
    // harness `dialoguer::Input` will fail with an io error which propagates
    // up — that's why the integration test for the no-flags branch only
    // asserts non-zero exit, not specific output.
    interactive_botfather_flow(bridge_id, kc.as_ref(), allow_groups).await
}

/// Five-step BotFather UX (Spec §M-c2.1):
///   1. Print BotFather URL + prompt for bot token.
///   2. Read token from stdin → write to keychain.
///   3. Generate nonce, print `t.me/<bot_username>?start=<nonce>`.
///   4. Wait for `/start <nonce>` from user's Telegram (30s timeout).
///   5. Write `telegram.yaml` + show E2E disclosure (typed-confirm).
///
/// Step 4 in M-c2.1 uses a stub: we do not poll Telegram getUpdates yet
/// (that lands in M-c2.2). Instead we prompt the user for the chat_id they
/// see logged on their side after sending `/start <nonce>`. The real polling
/// loop replaces this prompt in M-c2.2.
async fn interactive_botfather_flow(
    bridge_id: &str,
    kc: &dyn Keychain,
    allow_groups: Vec<i64>,
) -> Result<()> {
    use dialoguer::Input;

    // Step 1: BotFather URL + token prompt.
    println!(
        "Step 1/5 — open BotFather and create a new bot:\n  https://t.me/BotFather\n\
         Send /newbot and follow the prompts. When you receive your bot token, paste it below."
    );
    let bot_token: String = Input::new()
        .with_prompt("Bot token")
        .interact_text()
        .context("read bot token")?;

    // Step 2: persist token to keychain.
    let account = format!("{bridge_id}/telegram_bot_token");
    kc.put(&account, bot_token.trim())
        .context("write bot token to keychain")?;
    println!("Step 2/5 — token stored in keychain ({account}).");

    // Step 3: generate nonce + print pairing URL.
    let bot_username: String = Input::new()
        .with_prompt("Bot username (without leading @)")
        .interact_text()
        .context("read bot username")?;
    let nonce = generate_nonce();
    println!(
        "Step 3/5 — open this URL on the device with the user account that should pair with the bot:\n  \
         https://t.me/{bot_username}?start={nonce}"
    );

    // Step 4: wait for /start <nonce>. M-c2.1 stub: prompt for chat_id rather
    // than polling getUpdates. The real polling lands in M-c2.2.
    println!(
        "Step 4/5 — waiting up to 30s for the chat_id to be supplied (M-c2.1 stub: paste the chat_id manually for now)."
    );
    let chat_id_str: String = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(|| {
            Input::<String>::new()
                .with_prompt("chat_id (numeric, from your /start <nonce> message)")
                .interact_text()
        }),
    )
    .await
    .context("timed out waiting for /start <nonce> reply (30s)")?
    .context("blocking-thread join failed")?
    .context("read chat_id")?;
    let chat_id: i64 = chat_id_str
        .trim()
        .parse()
        .context("chat_id must be an integer")?;

    // Step 5: E2E disclosure + final scaffold.
    println!("Step 5/5 — Telegram E2E disclosure:\n{E2E_DISCLOSURE_TEXT}");
    let typed: String = Input::new()
        .with_prompt("Type 'I understand' to proceed")
        .interact_text()
        .context("read E2E disclosure ack")?;
    if !confirm_e2e_disclosure(typed.trim_end_matches('\n')) {
        bail!("telegram bridge requires E2E disclosure ack");
    }

    let outcome = scaffold_telegram_bridge(
        ScaffoldArgs {
            bridge_id: bridge_id.into(),
            bot_token: bot_token.trim().into(),
            bot_username,
            chat_id,
            ack: true,
            allow_groups,
        },
        kc,
    )?;
    let ScaffoldOutcome::Ok { profile_path, .. } = outcome;
    println!(
        "telegram bridge scaffolded; config: {}",
        profile_path.display()
    );
    Ok(())
}

/// Interactive 5-step Slack App setup wizard.
async fn run_slack_setup(bridge_id: &str) -> Result<()> {
    use std::io::Write;

    let kc: Box<dyn Keychain> =
        if std::env::var("MUR_SLACK_KEYCHAIN_BACKEND").ok().as_deref() == Some("mock") {
            Box::new(MockKeychain::default())
        } else {
            Box::new(SystemKeychain)
        };

    println!("\n━━ Slack Bridge Setup ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    println!(
        "Step 1/5  Create a Slack App\n\
         → https://api.slack.com/apps → Create New App → From scratch\n\
         Name: anything (e.g. \"{bridge_id}\"); pick your team's workspace.\n"
    );
    print!("          Press Enter when done… ");
    std::io::stdout().flush()?;
    let mut _buf = String::new();
    std::io::stdin().read_line(&mut _buf)?;

    println!(
        "\nStep 2/5  Enable Socket Mode + App Token\n\
         Settings → Socket Mode → Enable Socket Mode\n\
         Generate an App-level Token with scope: connections:write\n\
         Copy the token (starts with xapp-)\n"
    );
    print!("          App Token: ");
    std::io::stdout().flush()?;
    let mut app_token = String::new();
    std::io::stdin().read_line(&mut app_token)?;
    let app_token = app_token.trim().to_string();
    if !app_token.starts_with("xapp-") {
        anyhow::bail!("App Token must start with 'xapp-'");
    }

    println!(
        "\nStep 3/5  Add Bot Token Scopes\n\
         OAuth & Permissions → Bot Token Scopes → Add:\n\
           app_mentions:read  im:read  im:history  chat:write  users:read  channels:read\n"
    );
    print!("          Press Enter when done… ");
    std::io::stdout().flush()?;
    let mut _buf = String::new();
    std::io::stdin().read_line(&mut _buf)?;

    println!(
        "\nStep 4/5  Install App + Bot Token\n\
         OAuth & Permissions → Install to Workspace → Allow\n\
         Copy the Bot OAuth Token (starts with xoxb-)\n"
    );
    print!("          Bot Token: ");
    std::io::stdout().flush()?;
    let mut bot_token = String::new();
    std::io::stdin().read_line(&mut bot_token)?;
    let bot_token = bot_token.trim().to_string();
    if !bot_token.starts_with("xoxb-") {
        anyhow::bail!("Bot Token must start with 'xoxb-'");
    }

    print!("\nStep 5/5  Verifying… ");
    std::io::stdout().flush()?;
    let client = reqwest::Client::new();
    let resp = client
        .post("https://slack.com/api/auth.test")
        .bearer_auth(&bot_token)
        .send()
        .await
        .context("auth.test request failed")?;
    let body: serde_json::Value = resp.json().await.context("auth.test parse failed")?;
    if !body["ok"].as_bool().unwrap_or(false) {
        anyhow::bail!(
            "auth.test failed: {}",
            body["error"].as_str().unwrap_or("unknown")
        );
    }
    println!("auth.test ✓");

    let bot_account = format!("mur_slack_bot_{bridge_id}");
    let app_account = format!("mur_slack_app_{bridge_id}");
    kc.put(&bot_account, &bot_token)
        .context("storing bot token in keychain")?;
    kc.put(&app_account, &app_token)
        .context("storing app token in keychain")?;

    let agent_dir = crate::paths::mur_root(None).join("agents").join(bridge_id);
    let slack_config = SlackConfig {
        workspace_url: body["url"].as_str().unwrap_or("").to_string(),
        bot_token_keychain_account: bot_account,
        app_token_keychain_account: app_account,
        privacy_mode: SlackPrivacyMode::DmAndMentions,
        allowed_channels: vec![],
        allowed_user_ids: vec![],
    };
    let yaml = serde_yaml_ng::to_string(&slack_config).context("serialising slack.yaml")?;
    std::fs::write(agent_dir.join("slack.yaml"), yaml).context("writing slack.yaml")?;

    println!(
        "\n⚠  Privacy notice: This bridge is NOT end-to-end encrypted.\n\
         Messages are forwarded to your local mur agent over A2A.\n"
    );
    println!(
        "✅ Slack bridge configured for agent '{bridge_id}'.\n\
         Run: mur agent start {bridge_id}\n"
    );
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    Ok(())
}

fn generate_nonce() -> String {
    use rand::Rng;
    use rand::distributions::Alphanumeric;
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect()
}

/// Inputs to `scaffold_telegram_bridge`. Constructed by both the interactive
/// CLI flow and the non-interactive flag path; the integration tests build
/// these directly.
pub struct ScaffoldArgs {
    pub bridge_id: String,
    pub bot_token: String,
    pub bot_username: String,
    pub chat_id: i64,
    pub ack: bool,
    pub allow_groups: Vec<i64>,
}

/// Outcome from `scaffold_telegram_bridge`. Currently a single `Ok` variant —
/// the enum shape exists so future scaffold modes (rekey, repair) can extend
/// it without breaking call sites.
///
/// `config` is exposed for tests / callers that want to inspect the resulting
/// `TelegramConfig` without re-reading the YAML; the bin target's CLI flow
/// only uses `profile_path`, hence the allow.
pub enum ScaffoldOutcome {
    Ok {
        #[allow(dead_code)]
        config: TelegramConfig,
        profile_path: PathBuf,
    },
}

/// Library-level entry point for Telegram bridge scaffolding. Persists the bot
/// token to `kc` (the keychain), writes `telegram.yaml` under
/// `$MUR_HOME/agents/<bridge_id>/`, and returns the resolved `TelegramConfig`.
///
/// Hard-gated on `args.ack == true` (M-c2.1.3) — refuses to write anything
/// without an explicit E2E disclosure ack.
pub fn scaffold_telegram_bridge(args: ScaffoldArgs, kc: &dyn Keychain) -> Result<ScaffoldOutcome> {
    if !args.ack {
        bail!("telegram bridge requires E2E disclosure ack");
    }
    let account = format!("{}/telegram_bot_token", args.bridge_id);
    kc.put(&account, &args.bot_token)
        .context("write bot token to keychain")?;
    let cfg = TelegramConfig {
        bot_username: args.bot_username,
        bot_token_keychain_account: account,
        chat_id: args.chat_id,
        privacy_mode: if args.allow_groups.is_empty() {
            PrivacyMode::DmOnly
        } else {
            PrivacyMode::AllowGroups
        },
        allow_groups: args.allow_groups,
        e2e_disclosure_acked_at: Some(chrono::Utc::now()),
    };
    let profile_path = write_bridge_profile(&args.bridge_id, &cfg)?;
    Ok(ScaffoldOutcome::Ok {
        config: cfg,
        profile_path,
    })
}

fn write_bridge_profile(bridge_id: &str, cfg: &TelegramConfig) -> Result<PathBuf> {
    let dir = crate::paths::mur_root(None).join("agents").join(bridge_id);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("telegram.yaml");
    std::fs::write(&path, serde_yaml_ng::to_string(cfg)?)
        .with_context(|| format!("write {}", path.display()))?;
    // M-c2.5.3: register the bridge's outbound `chat.send_message` MCP tool
    // so user-agents can discover and spawn it via the standard
    // `profile.mcp_servers[]` surface. If `profile.yaml` already exists
    // (the normal flow goes through `scaffold_stub_bridge` first), we
    // round-trip the typed `AgentProfile` and append. If it doesn't exist
    // (test path / repair flows), we write a minimal partial YAML — that's
    // enough for the schema-tolerant loader to pick the entry up.
    upsert_telegram_chat_mcp_entry(&dir, bridge_id)?;
    Ok(path)
}

/// Add (or upsert by name) a `telegram_chat` entry into the bridge's
/// `profile.yaml#mcp_servers[]`. Idempotent — re-running the scaffold
/// won't create duplicate entries.
fn upsert_telegram_chat_mcp_entry(dir: &std::path::Path, bridge_id: &str) -> Result<()> {
    use mur_common::agent::McpServerEntry;

    let profile_path = dir.join("profile.yaml");
    let entry = McpServerEntry {
        name: "telegram_chat".into(),
        // The bridge runtime binary is symlinked as `mur_agent_<bridge_id>`
        // by the supervisor; user-agents resolve it by name on PATH (or
        // MUR_AGENT_BIN_DIR). Pass `mcp` to enter the stdio MCP server
        // mode added in M-c2.5.1.
        command: format!("mur_agent_{bridge_id}"),
        args: vec!["mcp".into()],
        // The bridge MCP is internal mur infrastructure and ships
        // alongside the runtime — pinning happens out-of-band via
        // codesign (rule 11) rather than user-driven `mur agent mcp pin`.
        ..Default::default()
    };

    if profile_path.exists() {
        // Round-trip the typed profile so we don't drift other fields.
        let yaml = std::fs::read_to_string(&profile_path)
            .with_context(|| format!("read {}", profile_path.display()))?;
        let mut profile: mur_common::AgentProfile = serde_yaml_ng::from_str(&yaml)
            .with_context(|| format!("parse {}", profile_path.display()))?;
        if let Some(existing) = profile
            .mcp_servers
            .iter_mut()
            .find(|e| e.name == entry.name)
        {
            *existing = entry;
        } else {
            profile.mcp_servers.push(entry);
        }
        std::fs::write(&profile_path, serde_yaml_ng::to_string(&profile)?)
            .with_context(|| format!("write {}", profile_path.display()))?;
    } else {
        // Test / repair path — no full profile yet. Write a minimal
        // partial YAML containing just the mcp_servers entry. Production
        // always reaches here through `scaffold_stub_bridge` first, so
        // this branch is only exercised by tests that drive
        // `scaffold_telegram_bridge` directly.
        let partial = serde_yaml_ng::to_string(&serde_yaml_ng::mapping::Mapping::from_iter([(
            serde_yaml_ng::Value::from("mcp_servers"),
            serde_yaml_ng::to_value(vec![entry])?,
        )]))?;
        std::fs::write(&profile_path, partial)
            .with_context(|| format!("write {}", profile_path.display()))?;
    }
    Ok(())
}

/// Literal-match gate for the Telegram E2E disclosure (M-c2.1.3). The user
/// must type the string `I understand` — case- and whitespace-sensitive.
pub fn confirm_e2e_disclosure(input: &str) -> bool {
    input == "I understand"
}

/// Disclosure text shown to the user before the literal-match prompt. Kept as
/// a module constant so doc and CLI surfaces share the same wording.
pub const E2E_DISCLOSURE_TEXT: &str = "\
Telegram chats are NOT end-to-end encrypted unless using Secret Chats. \
Bot messages traverse Telegram's servers in plaintext. \
The bot token has full read/send access to messages addressed to the bot. \
Type exactly 'I understand' to proceed.";

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

    mur_common::validate_agent_name(name)
        .with_context(|| format!("invalid bridge agent name {name:?}"))?;

    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("home dir resolvable").join(".mur"));
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
        role: None,
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
        fallback_chain: Vec::new(),
        routing: None,
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
            webhook: mur_common::agent::WebhookTransportConfig::default(),
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
            tools: vec![],
            fail_closed_on_sandbox_error: true,
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
            idle_triggers: Vec::new(),
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
        voice: mur_common::agent::VoiceConfig::default(),
        hooks: mur_common::HooksConfig::default(),
        trusted_peers: vec![],
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
