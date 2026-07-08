//! Extracted helpers for supervisor.rs — keeps it under 800 lines per CLAUDE.md §4.

use std::sync::Arc;

use anyhow::Context;
use tracing::{error, warn};

use crate::companion::clock::SystemClock;
use crate::hitl::HitlApprovals;
use crate::hooks::{HookChain, HookCtx, TelemetryEmitter};
use crate::llm::LlmClient;
use crate::llm::{anthropic::AnthropicClient, ollama::OllamaClient, openai::OpenAiClient};
use crate::mcp::pool::McpPool;
use crate::profile::Profile;
use crate::sandbox::SandboxPolicy;
use crate::sandbox::reqwest_guard::HostGuard;
use crate::skills::RuntimeSkills;
use crate::task_runner::TaskRunner;
use crate::telemetry_writer::{TelemetryWriter, WriterTelemetryEmitter};
use crate::tools::bash::BashTool;
use crate::tools::registry::build_tools;
use mur_common::agent::NetworkOutboundMode;
use mur_common::config::SkillsConfig;
use mur_common::model::ModelEntry;
use mur_common::skill::aggregator::{StatsAggregator, StatsEvent};
use mur_common::telemetry::{
    METHOD_SKILL_EXECUTED, MUR_SKILL_DURATION_MS, MUR_SKILL_MANIFEST_DIGEST, MUR_SKILL_NAME,
    MUR_SKILL_OUTCOME, MUR_SKILL_VERSION,
};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

/// Fallback base URL when neither the registry entry, the env var, nor the
/// shared file provides one (e.g. running outside Hub). Points at the
/// conventional local sidecar port.
pub(crate) const LOCAL_LLM_DEFAULT_BASE_URL: &str = "http://127.0.0.1:50320/v1";

/// Placeholder API key for the local OpenAI-compatible MLX server, which does
/// not authenticate. Not a secret.
pub(crate) const LOCAL_LLM_PLACEHOLDER_KEY: &str = "local-no-key";

/// The external host the agent's configured model talks to, for auto-allowing
/// it under restricted outbound (so a user never has to `allow-host` their own
/// provider). `None` for loopback base_urls (handled by `local_llm_port`) and
/// for entries without a base_url.
fn provider_host(entry: &ModelEntry) -> Option<String> {
    let base = entry.base_url.as_deref()?;
    let host = base.parse::<reqwest::Url>().ok()?.host_str()?.to_string();
    match host.as_str() {
        "127.0.0.1" | "localhost" | "::1" => None,
        _ => Some(host),
    }
}

/// The TCP port of the agent's own local LLM endpoint, if its resolved model
/// points at a loopback address. The runtime grants this port through the B1
/// sandbox so an agent can always reach its configured model — whatever the
/// provider or port (ollama 11434, bundled MLX 50320, an oMLX / LM Studio /
/// OpenAI-compatible server on any local port via `base_url`). Returns `None`
/// for remote endpoints (anthropic/openai cloud), which already use 443.
pub(crate) fn local_llm_port(
    profile: &mur_common::agent::AgentProfile,
    mur_home: &std::path::Path,
) -> Option<u16> {
    fn loopback_port(url: &str) -> Option<u16> {
        let u = url.parse::<reqwest::Url>().ok()?;
        match u.host_str()? {
            "127.0.0.1" | "localhost" | "::1" => u.port_or_known_default(),
            _ => None,
        }
    }

    // Prefer the resolved registry entry (honours `model_ref`, e.g. a
    // user-configured oMLX endpoint with an explicit base_url).
    if let Ok(entry) = crate::supervisor::resolve_model_entry(profile) {
        if let Some(base) = entry.base_url.as_deref()
            && let Some(p) = loopback_port(base)
        {
            return Some(p);
        }
        match entry.provider.as_str() {
            "ollama" => {
                return loopback_port(
                    &std::env::var("OLLAMA_BASE_URL")
                        .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string()),
                );
            }
            "local" => {
                return loopback_port(&resolve_local_base_url(
                    None,
                    std::env::var("MUR_LOCAL_LLM_BASE_URL").ok(),
                    mur_home,
                ));
            }
            // Cloud providers routed through a local bridge (e.g. an OAuth proxy
            // / cc-proxy) via their conventional base-URL env var. Remote
            // endpoints (https://api.anthropic.com) are not loopback, so
            // `loopback_port` returns `None` and cloud behaviour is unchanged.
            "anthropic" => {
                if let Ok(base) = std::env::var("ANTHROPIC_BASE_URL")
                    && let Some(p) = loopback_port(&base)
                {
                    return Some(p);
                }
            }
            "openai" => {
                if let Ok(base) = std::env::var("OPENAI_BASE_URL")
                    && let Some(p) = loopback_port(&base)
                {
                    return Some(p);
                }
            }
            _ => {}
        }
    }
    None
}

/// Resolve the local model base URL: entry.base_url → env → shared file → default.
pub(crate) fn resolve_local_base_url(
    entry_base_url: Option<&str>,
    env_base_url: Option<String>,
    mur_home: &std::path::Path,
) -> String {
    if let Some(u) = entry_base_url {
        return u.to_string();
    }
    if let Some(u) = env_base_url {
        return u;
    }
    if let Some(u) = mur_common::local_llm::read_base_url(mur_home) {
        return u;
    }
    LOCAL_LLM_DEFAULT_BASE_URL.to_string()
}

#[allow(clippy::too_many_arguments)]
pub fn build_runner(
    client: Arc<dyn LlmClient>,
    base_system_prompt: Option<String>,
    skills: Arc<RuntimeSkills>,
    skills_cfg: SkillsConfig,
    hook_chain: Option<Arc<HookChain>>,
    hook_ctx: Option<HookCtx>,
    hook_cancel: Option<CancellationToken>,
    pending_approvals: Option<HitlApprovals>,
    notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    hitl_timeout_secs: u32,
    tools: Vec<std::sync::Arc<dyn crate::tools::ToolExecutor>>,
    tools_policy: Vec<mur_common::agent::ToolRule>,
    max_iterations: Option<u32>,
    max_tokens: Option<u64>,
) -> Arc<TaskRunner> {
    let mut runner = TaskRunner::with_llm(client)
        .with_system_prompt(base_system_prompt)
        .with_skills(skills)
        .with_skills_cfg(skills_cfg)
        .with_hitl_timeout_secs(hitl_timeout_secs)
        .with_tools(tools)
        .with_tools_policy(tools_policy);
    if let Some(n) = max_iterations {
        runner = runner.with_max_iterations(n);
    }
    if let Some(n) = max_tokens {
        runner = runner.with_max_token_budget(n);
    }
    if let (Some(chain), Some(ctx), Some(cancel)) = (hook_chain, hook_ctx, hook_cancel) {
        runner = runner.with_hook_chain(chain, ctx, cancel);
    }
    if let Some(pa) = pending_approvals {
        runner = runner.with_pending_approvals(pa);
    }
    if let Some(notif) = notifier {
        runner = runner.with_notifier(notif);
    }
    Arc::new(runner)
}

/// Build the LLM-backed TaskRunner for a resolved model entry.
/// Returns (runner, optional LLM client for companion sharing, optional McpPool for shutdown).
#[allow(clippy::too_many_arguments)]
pub async fn build_provider_runner(
    force_echo: bool,
    agent_home: &std::path::Path,
    profile: &Profile,
    runtime_skills: Arc<RuntimeSkills>,
    skills_cfg: SkillsConfig,
    hook_chain: &HookChain,
    hook_ctx: &HookCtx,
    hook_cancel: &CancellationToken,
    pending_approvals: Option<HitlApprovals>,
    notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    hitl_timeout_secs: u32,
    max_iterations: Option<u32>,
    max_tokens: Option<u64>,
) -> anyhow::Result<(
    Arc<TaskRunner>,
    Option<Arc<dyn LlmClient>>,
    Option<Arc<McpPool>>,
)> {
    if force_echo {
        return Ok((Arc::new(TaskRunner::new_stub_echo()), None, None));
    }

    let resolved = crate::supervisor::resolve_model_entry(&profile.inner);
    if let Err(ref e) = resolved {
        warn!(error = %e, "model resolution failed; will fall back to echo");
    }
    let entry = resolved.unwrap_or_else(|_| ModelEntry {
        provider: "echo".into(),
        model: String::new(),
        base_url: None,
        secret: None,
        capabilities: vec![],
        params: serde_json::Value::Null,
        tier: None,
        cost_per_1k_tokens: None,
        ..Default::default()
    });

    let secret_value: Option<secrecy::SecretString> = match &entry.secret {
        Some(s) => match s.resolve().await {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(error = %e, "secret resolution failed; falling back to echo");
                None
            }
        },
        None => None,
    };

    let outbound = &profile.inner.entitlements.network.outbound;
    let host_guard = match outbound.mode {
        NetworkOutboundMode::Unrestricted => HostGuard::unrestricted(),
        NetworkOutboundMode::Restricted => {
            // Auto-allow the agent's configured LLM provider host: choosing a
            // provider implies permission to reach it, so the user never has to
            // `mur agent perm allow-host` their own model endpoint. Mirrors the
            // loopback-port auto-grant for local models (`local_llm_port`).
            let mut hosts = outbound.allow_hosts.clone();
            if let Some(h) = provider_host(&entry)
                && !hosts.iter().any(|x| x == &h)
            {
                hosts.push(h);
            }
            HostGuard::restricted(hosts)
        }
        NetworkOutboundMode::Off => HostGuard::off(),
    };
    let guarded_http = reqwest::ClientBuilder::new()
        .dns_resolver(std::sync::Arc::new(host_guard))
        .build()
        .context("failed to build guarded HTTP client")?;

    // Build MCP pool from the agent profile's configured servers, then discover
    // and filter tools concurrently before constructing the runner.
    let sandbox_policy = SandboxPolicy::from_entitlements(&profile.inner.entitlements, agent_home);
    // Phase-1 enable/disable: drop servers disabled for this agent so they
    // are never spawned and never advertised in tools/list.
    let enabled_mcp = profile.inner.enabled_mcp_servers();
    // Start the per-server egress proxy only if some server declares a
    // Restricted or BroadAudited network policy (opt-in; otherwise no proxy,
    // no change).
    let needs_egress = enabled_mcp.iter().any(|e| {
        matches!(
            e.network.as_ref().map(|n| n.mode),
            Some(mur_common::agent::McpNetMode::Restricted)
                | Some(mur_common::agent::McpNetMode::BroadAudited)
        )
    });
    let egress = if needs_egress {
        match crate::sandbox::egress_proxy::start_egress_proxy().await {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::warn!(
                    "egress proxy failed to start; Restricted MCP servers will be unscoped: {e}"
                );
                None
            }
        }
    } else {
        None
    };
    let pool = McpPool::new(enabled_mcp.clone(), sandbox_policy, egress);
    let bash_exec: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(BashTool::new(agent_home.to_path_buf()));
    let bash_def = bash_exec.def();
    let read_file_exec: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::read_file::ReadFileTool::new(
            agent_home.to_path_buf(),
            profile.inner.entitlements.filesystem.clone(),
        ));
    let read_file_def = read_file_exec.def();
    let write_file_exec: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::write_file::WriteFileTool::new(
            agent_home.to_path_buf(),
            profile.inner.entitlements.filesystem.clone(),
        ));
    let write_file_def = write_file_exec.def();
    let edit_file_exec: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::edit_file::EditFileTool::new(
            agent_home.to_path_buf(),
            profile.inner.entitlements.filesystem.clone(),
        ));
    let edit_file_def = edit_file_exec.def();
    let tools_policy = profile.inner.entitlements.tools.clone();
    let (_defs, tool_map) = build_tools(
        Some((bash_def, bash_exec)),
        Some((read_file_def, read_file_exec)),
        Some((write_file_def, write_file_exec)),
        Some((edit_file_def, edit_file_exec)),
        &enabled_mcp,
        &tools_policy,
        pool.clone(),
    )
    .await;
    let tools: Vec<Arc<dyn crate::tools::ToolExecutor>> = tool_map.into_values().collect();

    let build = |client: Arc<dyn LlmClient>| {
        let r = crate::supervisor_runner::build_runner(
            client.clone(),
            profile.system_prompt.clone(),
            runtime_skills.clone(),
            skills_cfg.clone(),
            Some(Arc::new(hook_chain.clone())),
            Some(hook_ctx.clone()),
            Some(hook_cancel.clone()),
            pending_approvals.clone(),
            notifier.clone(),
            hitl_timeout_secs,
            tools.clone(),
            tools_policy.clone(),
            max_iterations,
            max_tokens,
        );
        (r, Some(client), Some(pool.clone()))
    };

    Ok(match entry.provider.as_str() {
        "local" => {
            let mur_home = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".mur");
            let base = resolve_local_base_url(
                entry.base_url.as_deref(),
                std::env::var("MUR_LOCAL_LLM_BASE_URL").ok(),
                &mur_home,
            );
            let key = secrecy::SecretString::from(LOCAL_LLM_PLACEHOLDER_KEY.to_string());
            let client = Arc::new(OpenAiClient::from_secret_string_with_http(
                &key,
                entry.model.clone(),
                Some(base),
                guarded_http,
            ));
            build(client)
        }
        "ollama" => {
            let base = entry.base_url.clone().unwrap_or_else(|| {
                std::env::var("OLLAMA_BASE_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string())
            });
            let client = Arc::new(OllamaClient::with_http_client(
                base,
                entry.model,
                guarded_http,
            ));
            build(client)
        }
        "anthropic" => {
            let built: Result<Arc<dyn LlmClient>, _> = if let Some(key) = secret_value.as_ref() {
                Ok(Arc::new(AnthropicClient::from_secret_string_with_http(
                    key,
                    entry.model.clone(),
                    entry.base_url.clone(),
                    guarded_http,
                )))
            } else {
                AnthropicClient::from_agent_credentials_with_http(
                    &profile.inner.name,
                    entry.model.clone(),
                    guarded_http,
                )
                .await
                .map(|c| Arc::new(c) as Arc<dyn LlmClient>)
            };
            match built {
                Ok(client) => build(client),
                Err(e) => {
                    warn!(error = %e, "anthropic client unavailable; falling back to echo");
                    (
                        Arc::new(TaskRunner::new_stub_echo()),
                        None,
                        Some(pool.clone()),
                    )
                }
            }
        }
        "openai" => {
            let built: Result<Arc<dyn LlmClient>, _> = if let Some(key) = secret_value.as_ref() {
                Ok(Arc::new(OpenAiClient::from_secret_string_with_http(
                    key,
                    entry.model.clone(),
                    entry.base_url.clone(),
                    guarded_http,
                )))
            } else {
                OpenAiClient::from_agent_credentials_with_http(
                    &profile.inner.name,
                    entry.model.clone(),
                    guarded_http,
                )
                .await
                .map(|c| Arc::new(c) as Arc<dyn LlmClient>)
            };
            match built {
                Ok(client) => build(client),
                Err(e) => {
                    warn!(error = %e, "openai client unavailable; falling back to echo");
                    (
                        Arc::new(TaskRunner::new_stub_echo()),
                        None,
                        Some(pool.clone()),
                    )
                }
            }
        }
        "echo" => {
            // Intentional fallback: model resolution failed or the agent has no
            // model configured. Degrade to echo (warned at resolution time).
            (
                Arc::new(TaskRunner::new_stub_echo()),
                None,
                Some(pool.clone()),
            )
        }
        other => {
            // A real provider was configured but this runtime ships no client for
            // it (e.g. `deepseek`). Do NOT silently echo — that looks alive but
            // parrots input. Surface the misconfiguration in the logs AND in every
            // chat reply so the user sees exactly what to change.
            let msg = format!(
                "⚠️ This agent's model provider '{other}' is not supported by the \
                 MUR runtime (supported: local, ollama, anthropic, openai). \
                 Update the agent's model to a supported provider in ~/.mur/models.yaml."
            );
            error!(provider = %other, "unsupported model provider — replying with misconfiguration notice instead of echo");
            (
                Arc::new(TaskRunner::new_stub_misconfigured(msg)),
                None,
                Some(pool.clone()),
            )
        }
    })
}

/// Telemetry writer + notification routing + hook chain + skills loaded once at boot.
pub(crate) async fn prepare_runtime(
    agent_home: &std::path::Path,
    profile: &Profile,
    socket_enabled: bool,
) -> anyhow::Result<(
    TelemetryWriter,
    tokio::sync::mpsc::Receiver<serde_json::Value>,
    tokio::sync::mpsc::Receiver<serde_json::Value>,
    tokio::sync::mpsc::Sender<serde_json::Value>,
    HookChain,
    HookCtx,
    CancellationToken,
    Arc<RuntimeSkills>,
    SkillsConfig,
)> {
    let (writer, notif_rx) = TelemetryWriter::new(
        agent_home.join("telemetry"),
        profile.inner.name.clone(),
        profile.inner.id.clone(),
    )
    .await?;

    // Notification routing: Event → serde_json::Value channels for transports.
    let (stdio_notif_tx, stdio_notif_rx) = tokio::sync::mpsc::channel(256);
    let (sock_notif_tx, sock_notif_rx) = tokio::sync::mpsc::channel(256);
    // A clone for the message/send handler to stream token deltas over the same
    // socket-notification channel that telemetry events use.
    let sock_notif_tx_for_dispatch = sock_notif_tx.clone();

    // M5a: stats aggregator — flushes skill execution counters to
    // ~/.mur/skills/<name>/stats.json sidecars on a 64-event / 2 s tick.
    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"));
    let (stats_tx, stats_rx) = tokio::sync::mpsc::channel::<StatsEvent>(256);
    let _stats_aggregator = StatsAggregator::spawn(mur_home.clone(), stats_rx);

    tokio::spawn(async move {
        let mut rx = notif_rx;
        while let Some(n) = rx.recv().await {
            let method = n.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let v = serde_json::to_value(&n).unwrap_or_default();
            if socket_enabled {
                let _ = sock_notif_tx.send(v).await;
            } else {
                let _ = stdio_notif_tx.send(v).await;
            }
            // Fan-out: forward skill execution events to the stats aggregator.
            if method == METHOD_SKILL_EXECUTED {
                let p = &n["params"];
                let skill_name = p[MUR_SKILL_NAME].as_str().unwrap_or("").to_string();
                let skill_version = p[MUR_SKILL_VERSION].as_str().unwrap_or("").to_string();
                let manifest_digest = p[MUR_SKILL_MANIFEST_DIGEST]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let outcome = p[MUR_SKILL_OUTCOME].as_str().unwrap_or("not_evaluated");
                let _duration_ms = p[MUR_SKILL_DURATION_MS].as_u64().unwrap_or(0);
                if !skill_name.is_empty() {
                    let _ = stats_tx
                        .send(StatsEvent {
                            skill_name,
                            skill_version,
                            manifest_digest,
                            success: outcome == "success",
                            failure: outcome == "failure",
                            now: chrono::Utc::now(),
                        })
                        .await;
                }
            }
        }
    });

    let telemetry_emitter: Arc<dyn TelemetryEmitter> =
        Arc::new(WriterTelemetryEmitter::new(writer.sender()));
    let hook_chain = crate::hooks::builder::build_chain(&profile.inner, agent_home, &mur_home);
    let mcp_server_binaries: Vec<std::path::PathBuf> = profile
        .inner
        .mcp_servers
        .iter()
        .filter_map(|s| {
            // PATH-resolve so the B0 signature/pin checks (rules 6 & 11) inspect
            // the same binary `Command::new` will spawn. A bare `node`/`npx`
            // taken verbatim is a CWD-relative path that doesn't exist, which
            // silently skips both checks. Unresolvable → drop (treated as
            // "uninstalled", matching the soft-fail behaviour in rule 6).
            let prog = s.command.split_whitespace().next().unwrap_or(&s.command);
            mur_common::exec::resolve_command(prog).ok()
        })
        .collect();
    let hook_ctx = HookCtx {
        agent_name: profile.inner.name.clone(),
        agent_uuid: profile.inner.id.clone(),
        run_id: format!("supervisor-{}", uuid::Uuid::now_v7()),
        clock: Arc::new(SystemClock),
        telemetry: telemetry_emitter.clone(),
        agent_home: agent_home.to_path_buf(),
        turn_id: 0,
        turn_flags: Vec::new(),
        entitlements: profile.inner.entitlements.clone(),
        mcp_server_binaries,
    };
    let hook_cancel = CancellationToken::new();
    hook_chain
        .on_startup(&hook_ctx, &profile.inner, &hook_cancel)
        .await;

    let skills_cfg = mur_common::config::Config::load_or_default(&mur_home).skills;
    let loaded = mur_common::skill::loader::load_all(&mur_home, &profile.inner.name);
    let loaded: Vec<_> = loaded
        .into_iter()
        .filter(|s| profile.inner.skill_enabled(&s.name))
        .collect();
    let runtime_skills = Arc::new(RuntimeSkills::build(loaded));

    Ok((
        writer,
        stdio_notif_rx,
        sock_notif_rx,
        sock_notif_tx_for_dispatch,
        hook_chain,
        hook_ctx,
        hook_cancel,
        runtime_skills,
        skills_cfg,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_host_extracts_external_host_only() {
        let mk = |b: Option<&str>| ModelEntry {
            base_url: b.map(Into::into),
            ..Default::default()
        };
        assert_eq!(
            provider_host(&mk(Some("https://api.deepseek.com"))).as_deref(),
            Some("api.deepseek.com")
        );
        // path on the base_url doesn't change the host
        assert_eq!(
            provider_host(&mk(Some("https://api.deepseek.com/v1"))).as_deref(),
            Some("api.deepseek.com")
        );
        // loopback endpoints are handled by local_llm_port, not allow_hosts
        assert_eq!(provider_host(&mk(Some("http://127.0.0.1:8088"))), None);
        assert_eq!(provider_host(&mk(Some("http://localhost:8000/v1"))), None);
        // no base_url => nothing to auto-allow
        assert_eq!(provider_host(&mk(None)), None);
    }

    #[test]
    fn local_base_url_prefers_entry_then_env_then_file_then_default() {
        use std::path::Path;
        // entry wins
        assert_eq!(
            resolve_local_base_url(Some("http://e/v1"), None, Path::new("/nonexistent")),
            "http://e/v1"
        );
        // env wins when entry absent
        assert_eq!(
            resolve_local_base_url(
                None,
                Some("http://env/v1".into()),
                Path::new("/nonexistent")
            ),
            "http://env/v1"
        );
        // default when nothing available
        assert_eq!(
            resolve_local_base_url(None, None, Path::new("/nonexistent")),
            LOCAL_LLM_DEFAULT_BASE_URL
        );
    }

    /// Build a minimal inline-model `AgentProfile` (no `model_ref`, so
    /// `resolve_model_entry` never touches the registry) for the given provider.
    fn inline_profile(provider: &str, name: &str) -> mur_common::agent::AgentProfile {
        const MINIMAL: &str = r#"
schema: 1
id: 0192f5a1-28ab-7111-8000-000000000002
name: agent_a
display_name: "Agent A"
version: "0.1.0"
persona:
  category: research
  description: "Minimal test agent"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "m", params: {} }
mcp_servers: []
skills: []
transport: { stdio: true, socket: { enabled: true, bind: "unix:///tmp/a.sock" } }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: ["a2a.message.send","a2a.tasks"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: ["~/.ssh"] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: ["rate_limit"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
"#;
        let mut p: mur_common::agent::AgentProfile =
            serde_yaml_ng::from_str(MINIMAL).expect("minimal profile parses");
        p.model.provider = provider.to_string();
        p.model.name = name.to_string();
        p.model_ref = None;
        p
    }

    /// A cloud provider routed through a loopback bridge via `ANTHROPIC_BASE_URL`
    /// must have its local proxy port allowlisted; a remote endpoint must not.
    /// Env-var mutations make this test order-sensitive, so it owns the variable
    /// for its full duration and clears it before exercising the negative cases.
    #[test]
    fn anthropic_via_local_bridge_grants_loopback_port() {
        let profile = inline_profile("anthropic", "claude-sonnet-4-6");
        let mur_home = std::path::Path::new("/nonexistent");

        // Local bridge → port is granted.
        unsafe {
            std::env::set_var("ANTHROPIC_BASE_URL", "http://127.0.0.1:8088");
        }
        assert_eq!(local_llm_port(&profile, mur_home), Some(8088));

        // Remote cloud endpoint → not loopback → no port granted.
        unsafe {
            std::env::set_var("ANTHROPIC_BASE_URL", "https://api.anthropic.com");
        }
        assert_eq!(local_llm_port(&profile, mur_home), None);

        // Unset → no port granted.
        unsafe {
            std::env::remove_var("ANTHROPIC_BASE_URL");
        }
        assert_eq!(local_llm_port(&profile, mur_home), None);
    }
}
