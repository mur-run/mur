//! Extracted helpers for supervisor.rs — keeps it under 800 lines per CLAUDE.md §4.

use std::sync::Arc;

use anyhow::Context;
use tracing::warn;

use crate::companion::clock::SystemClock;
use crate::hooks::{HookChain, HookCtx, TelemetryEmitter};
use crate::llm::LlmClient;
use crate::llm::{anthropic::AnthropicClient, ollama::OllamaClient, openai::OpenAiClient};
use crate::profile::Profile;
use crate::sandbox::reqwest_guard::HostGuard;
use crate::skills::RuntimeSkills;
use crate::task_runner::TaskRunner;
use crate::telemetry_writer::{TelemetryWriter, WriterTelemetryEmitter};
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

pub fn build_runner(
    client: Arc<dyn LlmClient>,
    base_system_prompt: Option<String>,
    skills: Arc<RuntimeSkills>,
    skills_cfg: SkillsConfig,
    hook_chain: Option<Arc<HookChain>>,
    hook_ctx: Option<HookCtx>,
    hook_cancel: Option<CancellationToken>,
) -> Arc<TaskRunner> {
    let mut runner = TaskRunner::with_llm(client)
        .with_system_prompt(base_system_prompt)
        .with_skills(skills)
        .with_skills_cfg(skills_cfg);
    if let (Some(chain), Some(ctx), Some(cancel)) = (hook_chain, hook_ctx, hook_cancel) {
        runner = runner.with_hook_chain(chain, ctx, cancel);
    }
    Arc::new(runner)
}

/// Build the LLM-backed TaskRunner for a resolved model entry.
/// Returns (runner, optional LLM client for companion sharing).
pub async fn build_provider_runner(
    force_echo: bool,
    profile: &Profile,
    runtime_skills: Arc<RuntimeSkills>,
    skills_cfg: SkillsConfig,
    hook_chain: &HookChain,
    hook_ctx: &HookCtx,
    hook_cancel: &CancellationToken,
) -> anyhow::Result<(Arc<TaskRunner>, Option<Arc<dyn LlmClient>>)> {
    if force_echo {
        return Ok((Arc::new(TaskRunner::new_stub_echo()), None));
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
        NetworkOutboundMode::Restricted => HostGuard::restricted(outbound.allow_hosts.clone()),
        NetworkOutboundMode::Off => HostGuard::off(),
    };
    let guarded_http = reqwest::ClientBuilder::new()
        .dns_resolver(std::sync::Arc::new(host_guard))
        .build()
        .context("failed to build guarded HTTP client")?;

    let build = |client: Arc<dyn LlmClient>| {
        let r = crate::supervisor_runner::build_runner(
            client.clone(),
            profile.system_prompt.clone(),
            runtime_skills.clone(),
            skills_cfg.clone(),
            Some(Arc::new(hook_chain.clone())),
            Some(hook_ctx.clone()),
            Some(hook_cancel.clone()),
        );
        (r, Some(client))
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
                    (Arc::new(TaskRunner::new_stub_echo()), None)
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
                    (Arc::new(TaskRunner::new_stub_echo()), None)
                }
            }
        }
        other => {
            warn!(provider = %other, "no LLM client implemented; falling back to echo");
            (Arc::new(TaskRunner::new_stub_echo()), None)
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
            s.command
                .split_whitespace()
                .next()
                .map(std::path::PathBuf::from)
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
    let runtime_skills = Arc::new(RuntimeSkills::build(loaded));

    Ok((
        writer,
        stdio_notif_rx,
        sock_notif_rx,
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
}
