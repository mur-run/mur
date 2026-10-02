//! Extracted helpers for supervisor/mod.rs — keeps it under 800 lines per CLAUDE.md §5.
//! `provider` builds the LLM-backed runner; `prepare` does boot-time wiring.

use std::sync::Arc;

use crate::hitl::HitlApprovals;
use crate::hooks::{HookChain, HookCtx};
use crate::skills::RuntimeSkills;
use crate::task_runner::TaskRunner;
use mur_common::config::SkillsConfig;
use mur_common::model::ModelEntry;
use tokio_util::sync::CancellationToken;

/// Fallback base URL when neither the registry entry, the env var, nor the
/// shared file provides one (e.g. running outside Hub). Points at the
/// conventional local sidecar port.
pub(crate) const LOCAL_LLM_DEFAULT_BASE_URL: &str = "http://127.0.0.1:50320/v1";
/// Ollama's default endpoint when `OLLAMA_BASE_URL` is unset.
const OLLAMA_DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";

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

/// Provider marker for "this agent's model reference did not resolve".
///
/// Deliberately NOT `echo`. A failure that borrows a legitimate value's
/// identity becomes indistinguishable from that value being chosen on
/// purpose, and that is precisely how a broken agent came up as a parrot:
/// resolution failed, the fallback wrote `provider: "echo"`, and nothing
/// downstream could tell "the user asked for a stub" from "this agent has
/// no model". `echo` is a useful stub; no model is a fault.
const UNRESOLVED_PROVIDER: &str = "unresolved";

/// What the user reads, in chat, from an agent that cannot answer.
///
/// The reply is the only surface they are looking at in the moment it
/// matters — a WARN in a multi-megabyte log is not an answer to "why is my
/// agent repeating me". Every command named here is one that exists.
fn no_model_notice(agent: &str, reason: &str) -> String {
    format!(
        "⚠️ This agent has no working model, so it cannot answer.\n\
         Reason: {reason}\n\
         Diagnose: `mur agent doctor {agent}`\n\
         Pick a model: `mur model list`, then `/model` in this chat\n\
         Apply it: `mur agent restart {agent}`"
    )
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

/// The TCP port of `url` when its host is loopback (`127.0.0.1`, `localhost`,
/// `::1`); `None` for remote hosts or unparsable URLs.
fn loopback_port(url: &str) -> Option<u16> {
    let u = url.parse::<reqwest::Url>().ok()?;
    match u.host_str()? {
        "127.0.0.1" | "localhost" | "::1" | "[::1]" => u.port_or_known_default(),
        _ => None,
    }
}

/// The loopback TCP port one model entry talks to, if any. Pure over its
/// inputs: `env` stands in for the process environment so callers (and
/// tests) decide where base-URL overrides come from. An explicit loopback
/// `base_url` wins; otherwise the provider's conventional default / env var
/// decides (ollama 11434, bundled MLX 50320, a cloud provider routed through a
/// local bridge via `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL`). Remote
/// endpoints return `None` — they go out on 443 through the general list.
pub(crate) fn entry_loopback_port(
    entry: &ModelEntry,
    env: &dyn Fn(&str) -> Option<String>,
    mur_home: &std::path::Path,
) -> Option<u16> {
    if let Some(base) = entry.base_url.as_deref()
        && let Some(p) = loopback_port(base)
    {
        return Some(p);
    }
    match entry.provider.as_str() {
        "ollama" => loopback_port(
            &env("OLLAMA_BASE_URL").unwrap_or_else(|| OLLAMA_DEFAULT_BASE_URL.to_string()),
        ),
        "local" => loopback_port(&resolve_local_base_url(
            None,
            env("MUR_LOCAL_LLM_BASE_URL"),
            mur_home,
        )),
        "anthropic" => env("ANTHROPIC_BASE_URL").and_then(|b| loopback_port(&b)),
        "openai" => env("OPENAI_BASE_URL").and_then(|b| loopback_port(&b)),
        _ => None,
    }
}

/// Every loopback LLM port this agent may dial during its lifetime: the
/// resolved model's port plus the port of every registry entry, because
/// `/model` hot-switch and `autopick_cheap` can move the agent to any
/// registry model AFTER the seal is applied (the seal cannot be widened
/// later). Sorted and de-duplicated. Pure over its inputs.
///
/// These are granted through the loopback carve-out (SBPL `localhost:port`),
/// never the general `*:port` list — a local LLM port has no business
/// reaching a remote host on the same port number.
pub(crate) fn llm_loopback_ports(
    current: Option<&ModelEntry>,
    registry: Option<&mur_common::model::ModelRegistry>,
    env: &dyn Fn(&str) -> Option<String>,
    mur_home: &std::path::Path,
) -> Vec<u16> {
    let mut ports: Vec<u16> = current
        .into_iter()
        .chain(registry.into_iter().flat_map(|r| r.models.values()))
        .filter_map(|e| entry_loopback_port(e, env, mur_home))
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// [`llm_loopback_ports`] against the live process: the profile's resolved
/// model, the on-disk registry (unreadable registry → current model only),
/// and the real environment.
pub(crate) fn local_llm_ports(
    profile: &mur_common::agent::AgentProfile,
    mur_home: &std::path::Path,
) -> Vec<u16> {
    let current = crate::supervisor::resolve_model_entry(profile).ok();
    let registry = mur_common::model::ModelRegistry::default_path()
        .ok()
        .and_then(|p| mur_common::model::ModelRegistry::load_from(&p).ok());
    llm_loopback_ports(
        current.as_ref(),
        registry.as_ref(),
        &|k| std::env::var(k).ok(),
        mur_home,
    )
}

/// The loopback port of the agent's CURRENT model only (no registry sweep).
#[cfg(test)]
pub(crate) fn local_llm_port(
    profile: &mur_common::agent::AgentProfile,
    mur_home: &std::path::Path,
) -> Option<u16> {
    let entry = crate::supervisor::resolve_model_entry(profile).ok()?;
    entry_loopback_port(&entry, &|k| std::env::var(k).ok(), mur_home)
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

/// A runner for the CLI-spawn track, or a misconfigured one that says why.
///
/// Every refusal here produces a turn the user can read. The alternative —
/// falling back to the echo stub — looks alive and parrots, which is the
/// failure `RunnerBackend::Misconfigured` was introduced to end.
fn cli_track_runner(
    backend: &'static mur_common::cli_backend::CliBackend,
    socket_bind: &str,
) -> TaskRunner {
    use mur_common::cli_backend::Activation;
    if let Activation::Disabled { reason } = backend.activation {
        return TaskRunner::new_stub_misconfigured(format!(
            "agent is on the `{}{}` track, which is disabled: {reason}",
            mur_common::cli_backend::PROVIDER_PREFIX,
            backend.key
        ));
    }
    let socket = socket_bind.trim_start_matches("unix://");
    if socket.is_empty() {
        return TaskRunner::new_stub_misconfigured(format!(
            "agent is on the `{}{}` track but has no unix socket configured; \
             the spawned CLI would have no way to reach MUR's tools",
            mur_common::cli_backend::PROVIDER_PREFIX,
            backend.key
        ));
    }
    TaskRunner::with_cli_spawn(backend).with_socket_path(std::path::PathBuf::from(socket))
}

#[allow(clippy::too_many_arguments)]
pub fn build_runner(
    // The runner to configure — `TaskRunner::with_llm` for the in-process
    // track, `with_cli_spawn` for the CLI track. Taken already-built rather
    // than as a client, because a CLI-spawn turn has no `LlmClient`: the CLI
    // owns the loop. Factoring it this way keeps ONE `with_*` chain; a second
    // copy is how a track ends up spawning a CLI whose tool calls arrive with
    // no policy attached.
    base: TaskRunner,
    base_system_prompt: Option<String>,
    skills: Arc<RuntimeSkills>,
    skills_cfg: SkillsConfig,
    memory_cfg: mur_common::config::MemoryConfig,
    hook_chain: Option<Arc<HookChain>>,
    hook_ctx: Option<HookCtx>,
    hook_cancel: Option<CancellationToken>,
    pending_approvals: Option<HitlApprovals>,
    notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    hitl_timeout_secs: u32,
    // Issue #001: how far a turn is carried before handing back. Same HITL
    // vocabulary as `hitl_timeout_secs`, and threaded the same way — through
    // ONE `with_*` chain, so the CLI-spawn track cannot end up with a
    // different continuation policy from the in-process one.
    autonomy: mur_common::hitl::Autonomy,
    tools: Vec<std::sync::Arc<dyn crate::tools::ToolExecutor>>,
    tools_policy: Vec<mur_common::agent::ToolRule>,
    // The two `limits:` scopes this process can see (spec §3.4).
    limits: (
        mur_common::limits::Limits,
        Option<mur_common::limits::Limits>,
    ),
    effort: Option<mur_common::llm::Effort>,
    // Where multi-turn memory is persisted so it survives a restart (#1199),
    // and the model's context window, which sizes the history budget (#1200).
    // `None` dir keeps the store in memory, as the stub runners want.
    conversation_dir: Option<std::path::PathBuf>,
    context_window: Option<u64>,
    agent_name: String,
    decision_store: Option<Arc<dyn crate::hitl::store::DecisionStore>>,
    // `None` for the stub runners, which have no tools to mask output from.
    secrets: Option<Arc<crate::secrets::SecretVault>>,
    // The tools' shared session cwd and the roots a turn may move it to, so
    // the prompt declares the working directory from the runtime's own state.
    session_cwd: Option<(crate::tools::fs_policy::SessionCwd, Vec<String>)>,
    // Shared with the bash tool: lets the loop end a task's jobs on an
    // unattended stop or a cancel (spec D3/D8). `None` for the stub runners.
    bash_jobs: Option<Arc<crate::tools::bash_jobs::JobTable>>,
    // The session cwd's AGENTS.md/CLAUDE.md, gated like `read_file`. Only the
    // in-process track reads the system prompt; a spawned CLI reads its own.
    project_instructions: Option<crate::project_instructions::ProjectInstructions>,
    // Is the B1 sandbox enforcing on this boot? `false` refuses every `Ask`
    // tool (D2 / D2b). No default on purpose: every caller must say.
    sandbox_enforcing: bool,
) -> Arc<TaskRunner> {
    let mut runner = base
        .with_agent_name(agent_name)
        .with_system_prompt(base_system_prompt)
        .with_skills(skills)
        .with_skills_cfg(skills_cfg)
        .with_memory_cfg(memory_cfg)
        .with_hitl_timeout_secs(hitl_timeout_secs)
        .with_autonomy(autonomy)
        .with_tools(tools)
        .with_tools_policy(tools_policy)
        .with_effort(effort);
    if let Some(v) = secrets {
        runner = runner.with_secrets(v);
    }
    if let Some(j) = bash_jobs {
        runner = runner.with_bash_jobs(j);
    }
    runner = runner
        .with_limits(limits.0, limits.1)
        .with_sandbox_enforcing(sandbox_enforcing);
    if let (Some(chain), Some(ctx), Some(cancel)) = (hook_chain, hook_ctx, hook_cancel) {
        runner = runner.with_hook_chain(chain, ctx, cancel);
    }
    if let Some(pa) = pending_approvals {
        runner = runner.with_pending_approvals(pa);
    }
    if let Some(notif) = notifier {
        runner = runner.with_notifier(notif);
    }
    if let Some(dir) = conversation_dir {
        runner = runner.with_conversation_memory(dir, context_window);
    }
    if let Some(s) = decision_store {
        runner = runner.with_decision_store(s);
    }
    if let Some((cwd, roots)) = session_cwd {
        runner = runner.with_session_cwd(cwd, roots);
    }
    if let Some(p) = project_instructions {
        runner = runner.with_project_instructions(p);
    }
    Arc::new(runner)
}

/// Does this agent need the routing-aware client, or is a single plain client
/// enough? Extracted so the decision is testable without building providers:
/// when this is wrong, Smart is silently inert for every agent with no
/// fallback chain and the toggle reports a state it does not have.
pub(crate) fn needs_routing_client(refs: usize, routing_on: bool, smart_on: bool) -> bool {
    refs > 1 || routing_on || smart_on
}
mod prepare;
mod provider;

pub(crate) use prepare::prepare_runtime;
pub use provider::build_provider_runner;

/// MUR's attribution rule for this agent's system prompt, or `None` when it
/// would be dead weight.
///
/// Two gates, and the capability one is the interesting half. The signature is
/// only ever applied by the agent shelling out to `gh`/`git`, so an agent with
/// `processes.spawn.mode: none` cannot obey the rule no matter how it is
/// configured — injecting it there spends context-window tokens on an
/// instruction that is unreachable, and teaches a chat-only agent about a PR
/// workflow it has no way to perform. Every other mode (`any`, `allowlist`,
/// `strict`) can exec *something*, and whether `git` is among the allowlisted
/// binaries is a runtime denial we deliberately do not try to predict here:
/// guessing wrong in that direction silently drops the signature from an agent
/// that would have applied it.
fn attribution_fragment(
    cfg: &mur_common::config::AttributionConfig,
    spawn_mode: mur_common::agent::SpawnMode,
) -> Option<String> {
    if spawn_mode == mur_common::agent::SpawnMode::None {
        return None;
    }
    cfg.prompt_fragment()
}

#[cfg(test)]
mod attribution_gate_tests {
    use super::attribution_fragment;
    use mur_common::agent::SpawnMode;
    use mur_common::config::AttributionConfig;

    /// An agent that can exec processes can reach `git`/`gh`, so the rule is
    /// worth its tokens.
    #[test]
    fn an_agent_that_can_spawn_processes_gets_the_rule() {
        let cfg = AttributionConfig::default();
        for mode in [SpawnMode::Any, SpawnMode::Allowlist, SpawnMode::Strict] {
            let f = attribution_fragment(&cfg, mode);
            assert!(
                f.as_deref().unwrap_or_default().contains("gh pr create"),
                "{mode:?} can exec, so it can open a PR"
            );
        }
    }

    /// A chat-only agent — `spawn: none` — can never run `gh` or `git`, so the
    /// rule could only ever be dead weight in its context window.
    #[test]
    fn a_chat_only_agent_is_not_charged_for_a_rule_it_cannot_follow() {
        let cfg = AttributionConfig::default();
        assert_eq!(attribution_fragment(&cfg, SpawnMode::None), None);
    }

    /// The config's off switch still wins over a capable agent.
    #[test]
    fn switching_both_surfaces_off_beats_the_capability_gate() {
        let cfg = AttributionConfig {
            pr: Some(String::new()),
            commit: None,
        };
        assert_eq!(attribution_fragment(&cfg, SpawnMode::Any), None);
    }
}

#[cfg(test)]
mod tests;
