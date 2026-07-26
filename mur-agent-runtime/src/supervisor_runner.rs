//! Extracted helpers for supervisor.rs — keeps it under 800 lines per CLAUDE.md §4.

use std::sync::Arc;

use tracing::{error, warn};

use crate::companion::clock::SystemClock;
use crate::hitl::HitlApprovals;
use crate::hooks::{HookChain, HookCtx, TelemetryEmitter};
use crate::llm::LlmClient;
use crate::mcp::pool::McpPool;
use crate::profile::Profile;
use crate::sandbox::SandboxPolicy;
use crate::skills::RuntimeSkills;
use crate::task_runner::TaskRunner;
use crate::telemetry_writer::{TelemetryWriter, WriterTelemetryEmitter};
use crate::tools::bash::BashTool;
use crate::tools::registry::build_tools;
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

/// True when any enabled MCP server declares a scoped network policy
/// (`Restricted` / `BroadAudited`) — i.e. the loopback egress proxy is
/// needed. Called by `supervisor::entrypoint()` BEFORE the kernel sandbox
/// seals, so the proxy's listener port can be carved into the profile
/// (a post-seal ephemeral port is unreachable to sandboxed children —
/// the G1 root cause).
pub(crate) fn profile_needs_egress(entries: &[mur_common::agent::McpServerEntry]) -> bool {
    entries.iter().any(|e| {
        matches!(
            e.network.as_ref().map(|n| n.mode),
            Some(mur_common::agent::McpNetMode::Restricted)
                | Some(mur_common::agent::McpNetMode::BroadAudited)
        )
    })
}

/// The external host the agent's configured model talks to, for auto-allowing
/// it under restricted outbound (so a user never has to `allow-host` their own
/// provider). `None` for loopback base_urls (handled by `local_llm_port`) and
/// for entries without a base_url.
pub(crate) fn provider_host(entry: &ModelEntry) -> Option<String> {
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
    egress_proxy: Option<crate::sandbox::egress_proxy::EgressProxyHandle>,
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
    // Routing telemetry sink (Phase B, Task 5) — `Some(writer.sender())` from
    // the caller's already-constructed `TelemetryWriter`. Only the routed
    // (`FallbackLlmClient`) path below records `Event::Routing`; the
    // single-model path has nothing to route between, so it's left alone.
    telemetry: Option<tokio::sync::mpsc::Sender<crate::telemetry_writer::Event>>,
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

    // Build MCP pool from the agent profile's configured servers, then discover
    // and filter tools concurrently before constructing the runner.
    let sandbox_policy = SandboxPolicy::from_entitlements(&profile.inner.entitlements, agent_home);
    // Phase-1 enable/disable: drop servers disabled for this agent so they
    // are never spawned and never advertised in tools/list.
    let enabled_mcp = profile.inner.enabled_mcp_servers();
    // The egress proxy (if any) was started by supervisor::entrypoint()
    // BEFORE the kernel sandbox sealed, so its port is carved into the
    // profile and sandboxed children can dial it. See profile_needs_egress.
    let egress = egress_proxy;
    let pool = McpPool::new(enabled_mcp.clone(), sandbox_policy, egress);
    // MUR_HOME-aware home dir — same expression as `prepare_runtime`/`supervisor.rs`
    // (the old inline "local"-arm recompute below ignored MUR_HOME; unifying on
    // this shared value is a disclosed, intentional bug-fix — see task-7-report.md).
    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"));

    // Issue #591 / runtime-file-tools-cwd: bash and the three file tools share
    // ONE session cwd (initial value = agent_home). The bash tool's `cwd`
    // parameter updates it; file tools resolve relative paths against the
    // current snapshot. A `cd` inside a bash subprocess is NOT retained.
    let session_cwd = crate::tools::fs_policy::SessionCwd::new(agent_home.to_path_buf());
    let bash_exec: Arc<dyn crate::tools::ToolExecutor> = Arc::new(
        BashTool::new(agent_home.to_path_buf(), session_cwd.clone())
            .with_agent(mur_home.clone(), profile.inner.name.clone()),
    );
    let bash_def = bash_exec.def();
    // Issue #712: the file tools must never touch the agent's own
    // profile.yaml / identity.key, whatever the profile grants.
    let tool_fs = crate::tools::fs_policy::self_protected(
        profile.inner.entitlements.filesystem.clone(),
        agent_home,
    );
    let read_file_exec: Arc<dyn crate::tools::ToolExecutor> = Arc::new(
        crate::tools::read_file::ReadFileTool::new(session_cwd.clone(), tool_fs.clone()),
    );
    let read_file_def = read_file_exec.def();
    let write_file_exec: Arc<dyn crate::tools::ToolExecutor> = Arc::new(
        crate::tools::write_file::WriteFileTool::new(session_cwd.clone(), tool_fs.clone()),
    );
    let write_file_def = write_file_exec.def();
    let edit_file_exec: Arc<dyn crate::tools::ToolExecutor> = Arc::new(
        crate::tools::edit_file::EditFileTool::new(session_cwd, tool_fs),
    );
    let edit_file_def = edit_file_exec.def();
    let tools_policy = profile.inner.entitlements.tools.clone();
    let (_defs, mut tool_map) = build_tools(
        Some((bash_def, bash_exec)),
        Some((read_file_def, read_file_exec)),
        Some((write_file_def, write_file_exec)),
        Some((edit_file_def, edit_file_exec)),
        &enabled_mcp,
        &tools_policy,
        pool.clone(),
    )
    .await;

    // Built-in fleet_run: registered ONLY for agents allowlisted in the global
    // config (`fleet_run.agents`, deny-by-default) — unauthorized agents never
    // see the tool. An explicit Deny rule in the profile still wins.
    {
        use crate::tools::fleet_run::{FLEET_RUN, FleetRunTool, agent_enabled};
        use mur_common::agent::{ToolPolicy, resolve_tool_policy};
        if agent_enabled(&mur_home, &profile.inner.name)
            && resolve_tool_policy(&tools_policy, FLEET_RUN) != ToolPolicy::Deny
        {
            tool_map.insert(
                FLEET_RUN.to_string(),
                Arc::new(FleetRunTool {
                    mur_home: mur_home.clone(),
                    agent_name: profile.inner.name.clone(),
                }),
            );
        }
    }
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

    // Model-switch: load the global config, resolve the ordered candidate refs
    // (per-agent overrides global) and decide single-client vs routing-aware
    // fallback chain. With no `models:` config and no per-agent chain/routing,
    // `refs.len() <= 1 && !routing.enabled` — the exact single-model path below
    // runs unchanged (byte-for-byte with the pre-Task-7 behaviour).
    let switch_cfg =
        mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).models;
    let routing = profile
        .inner
        .routing
        .clone()
        .unwrap_or_else(|| switch_cfg.routing.clone());
    let refs = mur_common::model::resolve_model_refs(&profile.inner, &switch_cfg, None);

    if refs.len() <= 1 && !routing.enabled {
        // Nothing configured (no chain, no routing) → today's exact single-model
        // path on the SAME `entry` resolved above via `resolve_model_entry`, no
        // FallbackLlmClient wrapper.
        return Ok(
            match crate::llm::client_builder::build_client_from_entry(&entry, profile, &mur_home) {
                Ok(client) => build(client),
                Err(e) => {
                    // A `guarded_http` build failure is a real error unrelated to
                    // provider support — pre-Task-7 this was a hard `?` straight
                    // out of this function, before the provider dispatch even
                    // ran. `local`/`ollama` have no arm below, so without this
                    // check such a failure would fall through to `other` and get
                    // mislabeled "unsupported model provider". Propagate it
                    // directly instead, restoring the original semantic.
                    if e.downcast_ref::<crate::llm::client_builder::GuardedHttpBuildError>()
                        .is_some()
                    {
                        return Err(e);
                    }
                    match entry.provider.as_str() {
                        "anthropic" => {
                            warn!(error = %e, "anthropic client unavailable; falling back to echo");
                            (
                                Arc::new(TaskRunner::new_stub_echo()),
                                None,
                                Some(pool.clone()),
                            )
                        }
                        "openai" => {
                            warn!(error = %e, "openai client unavailable; falling back to echo");
                            (
                                Arc::new(TaskRunner::new_stub_echo()),
                                None,
                                Some(pool.clone()),
                            )
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
                    }
                }
            },
        );
    }

    // Chain and/or routing configured → routing-aware fallback client.
    // Reusable per-ref client builder: model_ref -> Arc<dyn LlmClient>.
    // Reuses a fresh registry lookup (resolve_model_entry keys off
    // profile.model_ref; here we resolve an explicit candidate ref) +
    // build_client_from_entry (Step 1). Send + Sync: captures only a cloned
    // Profile and a cloned PathBuf.
    let profile_for_chain = profile.clone();
    let mur_home_for_chain = mur_home.clone();
    let build_one = move |model_ref: &str| -> anyhow::Result<Arc<dyn LlmClient>> {
        let reg = mur_common::model::ModelRegistry::load_from(
            &mur_common::model::ModelRegistry::default_path()?,
        )?;
        let candidate_entry = reg
            .models
            .get(model_ref)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("model_ref {model_ref:?} not in registry"))?;
        crate::llm::client_builder::build_client_from_entry(
            &candidate_entry,
            &profile_for_chain,
            &mur_home_for_chain,
        )
    };
    let fallback_client: Arc<dyn LlmClient> = {
        let mut fb = crate::llm::fallback::FallbackLlmClient::new_routed(
            profile.inner.clone(),
            switch_cfg.clone(),
            Box::new(build_one),
            switch_cfg.retry.clone(),
        );
        if let Some(tx) = telemetry {
            fb = fb.with_telemetry(tx, profile.inner.name.clone());
        }
        Arc::new(fb)
    };
    Ok(build(fallback_client))
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
    // #717: surface profile.yaml skill refs that don't resolve, distinguishing
    // missing (ref written but files never installed) from malformed (files
    // exist but no longer parse) so the log names the actual root cause.
    for r in &profile.inner.skills {
        use mur_common::skill::loader::SkillRefStatus;
        match mur_common::skill::loader::skill_ref_status(agent_home, r) {
            SkillRefStatus::Loadable => {}
            SkillRefStatus::Missing { path } => tracing::warn!(
                skill_ref = %r,
                path = %path.display(),
                "profile.yaml references a skill that is not installed (file not found); \
                 install it with `mur agent skill add` or remove the ref"
            ),
            SkillRefStatus::Malformed { path, error } => tracing::warn!(
                skill_ref = %r,
                path = %path.display(),
                error = %error,
                "profile.yaml references a skill whose file exists but no longer parses \
                 as a valid skill; re-install or remove it"
            ),
        }
    }
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
        let profile = inline_profile("anthropic", "claude-sonnet-5");
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

    #[test]
    fn profile_needs_egress_matches_scoped_modes() {
        use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};
        fn entry(mode: Option<McpNetMode>) -> McpServerEntry {
            let mut e = McpServerEntry {
                name: "s".into(),
                command: "cmd".into(),
                ..Default::default()
            };
            e.network = mode.map(|m| McpServerNetwork {
                mode: m,
                ..Default::default()
            });
            e
        }
        assert!(!profile_needs_egress(&[entry(None)]));
        assert!(!profile_needs_egress(&[entry(Some(McpNetMode::Inherit))]));
        assert!(!profile_needs_egress(&[entry(Some(McpNetMode::Off))]));
        assert!(profile_needs_egress(&[entry(Some(McpNetMode::Restricted))]));
        assert!(profile_needs_egress(&[
            entry(None),
            entry(Some(McpNetMode::BroadAudited))
        ]));
    }
}
