//! `build_provider_runner` and the model-chain hot-switch seam.

use std::sync::Arc;

use tracing::error;

use crate::hitl::HitlApprovals;
use crate::hooks::{HookChain, HookCtx};
use crate::llm::LlmClient;
use crate::mcp::pool::McpPool;
use crate::profile::Profile;
use crate::sandbox::SandboxPolicy;
use crate::skills::RuntimeSkills;
use crate::task_runner::TaskRunner;
use crate::tools::bash::BashTool;
use crate::tools::registry::build_tools;
use mur_common::config::SkillsConfig;
use mur_common::model::ModelEntry;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

use super::*;

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
    memory_cfg: mur_common::config::MemoryConfig,
    hook_chain: &HookChain,
    hook_ctx: &HookCtx,
    hook_cancel: &CancellationToken,
    pending_approvals: Option<HitlApprovals>,
    notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    hitl_timeout_secs: u32,
    // The two `limits:` scopes this process can see (spec §3.4).
    limits: (
        mur_common::limits::Limits,
        Option<mur_common::limits::Limits>,
    ),
    // Routing telemetry sink (Phase B, Task 5) — `Some(writer.sender())` from
    // the caller's already-constructed `TelemetryWriter`. Only the routed
    // (`FallbackLlmClient`) path below records `Event::Routing`; the
    // single-model path has nothing to route between, so it's left alone.
    telemetry: Option<tokio::sync::mpsc::Sender<crate::telemetry_writer::Event>>,
    // Supervisor's pre-sandbox-loaded keypair (#858: never lazy-load identity
    // after the sandbox applies) — signs memory proposals dropped by the
    // built-in remember tool (P2c-2).
    identity: Arc<mur_common::identity::AgentIdentity>,
    // Credentials the user handed this agent: exported into the bash tool's
    // children and masked out of every tool result. Loaded pre-seal by the
    // caller, because a keychain is unreachable once the sandbox closes.
    secrets: Arc<crate::secrets::SecretVault>,
    // B1 seal state for this boot, from the supervisor's `sandbox_record`.
    sandbox_enforcing: bool,
) -> anyhow::Result<(
    Arc<TaskRunner>,
    Option<Arc<dyn LlmClient>>,
    Option<Arc<McpPool>>,
    Option<Arc<crate::llm::switchable::ModelSwitchHandle>>,
)> {
    if force_echo {
        return Ok((Arc::new(TaskRunner::new_stub_echo()), None, None, None));
    }

    let resolved = crate::supervisor::resolve_model_entry(&profile.inner);
    // Keep the reason. A failure that borrows a legitimate provider's identity
    // is indistinguishable from that provider being chosen on purpose — which
    // is exactly how a broken agent came up as a parrot: resolution failed, the
    // fallback wrote `provider: "echo"`, and the `"echo"` arm below could no
    // longer tell "the user asked for a stub" from "this agent has no model".
    let unresolved: Option<String> = resolved.as_ref().err().map(|e| format!("{e:#}"));
    if let Some(ref e) = unresolved {
        error!(error = %e, "model resolution failed — the agent will report it, not echo");
    }
    let entry = resolved.unwrap_or_else(|_| ModelEntry {
        provider: UNRESOLVED_PROVIDER.into(),
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
    // MUR_HOME-aware home dir — same expression as `prepare_runtime`/`supervisor/mod.rs`
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
    let bash_jobs = crate::tools::bash_jobs::JobTable::new();
    let bash = Arc::new(
        BashTool::new(agent_home.to_path_buf(), session_cwd.clone())
            .with_agent(mur_home.clone(), profile.inner.name.clone())
            // From the profile already in memory: the agent cannot read its own
            // profile.yaml back (issue #712), so anything that re-reads it here
            // returns nothing and the denial goes unexplained.
            .with_write_grants(
                profile
                    .inner
                    .entitlements
                    .filesystem
                    .write
                    .iter()
                    .map(|w| crate::sandbox::policy::expand_entitlement_path(w))
                    .collect(),
            )
            .with_secrets(secrets.clone())
            .with_jobs(bash_jobs.clone()),
    );
    let bash_exec: Arc<dyn crate::tools::ToolExecutor> = bash.clone();
    let bash_def = bash_exec.def();
    // Issue #712: the file tools must never write the agent's own
    // SELF_PROTECTED_AGENT_FILES, whatever the profile grants.
    let tool_fs = crate::tools::fs_policy::for_file_tools(
        profile.inner.entitlements.filesystem.clone(),
        agent_home,
    );
    // Issue #712 protects this agent's own profile/key. The launch chain
    // protects every OTHER agent's, plus the binary and the autostart entries
    // that start them — none of which any entitlement may authorise.
    let launch_chain = crate::sandbox::launch_chain::LaunchChain::new(agent_home);
    let read_file_exec: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::read_file::ReadFileTool::new(
            session_cwd.clone(),
            tool_fs.clone(),
            launch_chain.clone(),
        ));
    let read_file_def = read_file_exec.def();
    let write_file_exec: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::write_file::WriteFileTool::new(
            session_cwd.clone(),
            tool_fs.clone(),
            launch_chain.clone(),
            profile.inner.name.clone(),
        ));
    let write_file_def = write_file_exec.def();
    let edit_file_exec: Arc<dyn crate::tools::ToolExecutor> =
        Arc::new(crate::tools::edit_file::EditFileTool::new(
            session_cwd.clone(),
            tool_fs.clone(),
            launch_chain.clone(),
            profile.inner.name.clone(),
        ));
    // The project's AGENTS.md/CLAUDE.md reach the prompt through the file
    // tools' own entitlement and launch chain — never a wider view than
    // `read_file` would give the model.
    let project_instructions =
        crate::project_instructions::ProjectInstructions::new(tool_fs, launch_chain);
    // Where a turn's `context.cwd` may move the session cwd: anything the
    // profile lets the agent read or write, plus its own home (the initial
    // value). Same `~` expansion as the tool gate, via `under_any`.
    let cwd_roots: Vec<String> = {
        let fs = &profile.inner.entitlements.filesystem;
        fs.read
            .iter()
            .chain(fs.write.iter())
            .cloned()
            .chain(std::iter::once(agent_home.to_string_lossy().into_owned()))
            .collect()
    };
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
    // bash_wait / bash_kill ride on bash's registration and policy (D6/D11).
    crate::tools::registry::attach_bash_control(&mut tool_map, bash.control_tools());

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
                    // Loaded before the sandbox sealed, which is the only
                    // reason we still have it: the child cannot read `keys/`.
                    signing: Some(identity.clone()),
                    key_version: profile.inner.identity.key_version,
                }),
            );
        }
    }
    // Built-in open_item: available to every agent, unlike fleet_run. Writing
    // a line into a log the user reads is not a capability worth gating, and
    // the display already marks everything it produces as unverified. An
    // explicit Deny in the profile still wins.
    {
        use crate::tools::open_item::{OPEN_ITEM, OpenItemTool};
        use mur_common::agent::{ToolPolicy, resolve_tool_policy};
        if resolve_tool_policy(&tools_policy, OPEN_ITEM) != ToolPolicy::Deny {
            tool_map.insert(
                OPEN_ITEM.to_string(),
                Arc::new(OpenItemTool {
                    mur_home: mur_home.clone(),
                    agent_name: profile.inner.name.clone(),
                }),
            );
        }
    }
    // Built-in remind: an agent cannot create its own schedule — its entries
    // live in a `profile.yaml` the sandbox denies it — so this writes a
    // proposal into the agent home instead. Available like `open_item` rather
    // than gated: it writes a file nothing acts on until a person runs
    // `mur agent schedule accept`, and the alternative is what #1075 recorded,
    // a timed request landing in a list with no clock. An explicit Deny still
    // wins.
    {
        use crate::tools::remind::{REMIND, RemindTool};
        use mur_common::agent::{ToolPolicy, resolve_tool_policy};
        if resolve_tool_policy(&tools_policy, REMIND) != ToolPolicy::Deny {
            tool_map.insert(
                REMIND.to_string(),
                Arc::new(RemindTool {
                    agent_home: agent_home.to_path_buf(),
                }),
            );
        }
    }
    // Built-in remember (memory federation P2a): proactive capture of durable
    // user preferences/facts as agent-local Draft notes. Gated on the global
    // `memory.capture` config; an explicit Deny in the profile still wins.
    let memory_capture = crate::tools::remember::capture_mode(&mur_home);
    {
        use crate::tools::remember::{REMEMBER, RememberTool};
        use mur_common::agent::{ToolPolicy, resolve_tool_policy};
        use mur_common::config::CaptureMode;
        if memory_capture != CaptureMode::Off
            && resolve_tool_policy(&tools_policy, REMEMBER) != ToolPolicy::Deny
        {
            tool_map.insert(
                REMEMBER.to_string(),
                Arc::new(RememberTool {
                    mur_home: mur_home.clone(),
                    agent_name: profile.inner.name.clone(),
                    identity: identity.clone(),
                    skills: runtime_skills.clone(),
                    // The turn's session cwd, not the process cwd: a runtime
                    // process lives in the agent home, which is never a repo,
                    // so resolving there made every `scope: project` save
                    // degrade to user scope.
                    active_project: crate::tools::remember::session_project_resolver(
                        session_cwd.clone(),
                    ),
                }),
            );
            // Registered with `remember`, under the same capture gate: an agent
            // allowed to save memories should be able to read them back, and
            // one with capture off has none of its own to read.
            use crate::tools::recall::{RECALL, RecallTool};
            if resolve_tool_policy(&tools_policy, RECALL) != ToolPolicy::Deny {
                tool_map.insert(
                    RECALL.to_string(),
                    Arc::new(RecallTool {
                        skills: runtime_skills.clone(),
                    }),
                );
            }
        }
    }
    let tools: Vec<Arc<dyn crate::tools::ToolExecutor>> = tool_map.into_values().collect();

    // Memory-capture directive (P2a): appended to the system prompt whenever
    // the remember tool is available, so the prompt contract and the tool's
    // presence never disagree. `ask` mode adds the confirm-first sentence.
    let system_prompt_with_memory: Option<String> = {
        use mur_common::config::CaptureMode;
        match memory_capture {
            CaptureMode::Off => profile.system_prompt.clone(),
            mode => {
                let mut p = profile.system_prompt.clone().unwrap_or_default();
                p.push_str(crate::tools::remember::MEMORY_DIRECTIVE);
                if mode == CaptureMode::Ask {
                    p.push_str(crate::tools::remember::MEMORY_DIRECTIVE_ASK);
                }
                Some(p)
            }
        }
    };

    // MUR's signature on PRs/commits this agent publishes. Appended here, beside
    // the memory directive, for the same reason: `config.yaml` is already loaded
    // on this path, so one source of truth feeds the prompt. Skill bodies cannot
    // do it — `skill::loader::load_all` reads them verbatim, with no template
    // expansion, so a signature written into skill markdown could never track
    // this config.
    let system_prompt_with_memory: Option<String> = {
        let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
        match attribution_fragment(
            &cfg.attribution,
            profile.inner.entitlements.processes.spawn.mode,
        ) {
            None => system_prompt_with_memory,
            Some(frag) => {
                let mut p = system_prompt_with_memory.unwrap_or_default();
                p.push_str(&frag);
                Some(p)
            }
        }
    };

    // P3: gate B's memory — settled chat-gate decisions, signed by this agent.
    let decision_store: Arc<dyn crate::hitl::store::DecisionStore> =
        Arc::new(crate::hitl::store::ChannelDecisionStore::new(
            mur_home.clone(),
            profile.inner.name.clone(),
            identity.clone(),
            profile.inner.identity.key_version,
        ));
    // One configuration chain, fed by either track. Split out so the CLI
    // branch below cannot drift from the in-process one: a runner missing its
    // tools policy would spawn a CLI whose calls arrive unpoliced.
    let build_base = |base: TaskRunner| {
        crate::supervisor_runner::build_runner(
            base,
            system_prompt_with_memory.clone(),
            runtime_skills.clone(),
            skills_cfg.clone(),
            memory_cfg.clone(),
            Some(Arc::new(hook_chain.clone())),
            Some(hook_ctx.clone()),
            Some(hook_cancel.clone()),
            pending_approvals.clone(),
            notifier.clone(),
            hitl_timeout_secs,
            profile.inner.hitl.autonomy.unwrap_or_default(),
            tools.clone(),
            tools_policy.clone(),
            limits,
            profile.inner.effort,
            Some(agent_home.join("conversations")),
            entry.context_window,
            profile.inner.name.clone(),
            Some(decision_store.clone()),
            Some(secrets.clone()),
            Some((session_cwd.clone(), cwd_roots.clone())),
            Some(bash_jobs.clone()),
            Some(project_instructions.clone()),
            sandbox_enforcing,
        )
    };
    // The CLI track short-circuits here: a spawned CLI owns the loop, so there
    // is no `LlmClient` to build and the routing below has nothing to route.
    // Its tools are the same ones — they reach the CLI through the shim.
    if let Some(backend) = mur_common::cli_backend::from_provider(&entry.provider) {
        let r = build_base(cli_track_runner(
            backend,
            &profile.inner.transport.socket.bind,
        ));
        return Ok((r, None, Some(pool.clone()), None));
    }

    let build = |client: Arc<dyn LlmClient>| {
        let r = build_base(TaskRunner::with_llm(client.clone()));
        (r, Some(client), Some(pool.clone()))
    };

    // Model-switch: load the global config, resolve the ordered candidate refs
    // (per-agent overrides global) and decide single-client vs routing-aware
    // fallback chain. With no `models:` config, no per-agent chain/routing and
    // Smart off (the default), `needs_routing_client` is false — the exact
    // single-model path below runs unchanged (byte-for-byte with the
    // pre-Task-7 behaviour).
    let switch_cfg =
        mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).models;
    let routing = profile.inner.effective_routing(&switch_cfg);
    let smart = profile.inner.effective_smart(&switch_cfg);
    let refs = mur_common::model::resolve_model_refs(&profile.inner, &switch_cfg, None);

    if !needs_routing_client(refs.len(), routing.enabled, smart.enabled) {
        // Nothing configured (no chain, no routing) → today's exact single-model
        // path on the SAME `entry` resolved above via `resolve_model_entry`, no
        // FallbackLlmClient wrapper.
        return Ok(
            match crate::llm::client_builder::build_client_from_entry(&entry, profile, &mur_home) {
                Ok(client) => {
                    // Hot-switch seam (murmur /model): wrap the client so
                    // `model/set` can replace it between turns, and hand the
                    // dispatcher a per-ref builder that reuses the exact boot
                    // construction path (fresh registry lookup + same profile).
                    let switchable = crate::llm::switchable::SwitchableLlmClient::new(client);
                    let profile_for_switch = profile.clone();
                    let mur_home_for_switch = mur_home.clone();
                    let build_client: crate::llm::fallback::ClientFactory =
                        Box::new(move |model_ref: &str| {
                            let reg = mur_common::model::ModelRegistry::load_from(
                                &mur_common::model::ModelRegistry::default_path()?,
                            )?;
                            let entry = reg.models.get(model_ref).cloned().ok_or_else(|| {
                                anyhow::anyhow!("model_ref {model_ref:?} not in registry")
                            })?;
                            crate::llm::client_builder::build_client_from_entry(
                                &entry,
                                &profile_for_switch,
                                &mur_home_for_switch,
                            )
                        });
                    let handle = Arc::new(crate::llm::switchable::ModelSwitchHandle {
                        switchable: switchable.clone(),
                        build_client,
                    });
                    let (r, c, p) = build(switchable as Arc<dyn LlmClient>);
                    (r, c, p, Some(handle))
                }
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
                        // A configured provider whose client would not build is
                        // broken, not a stub: say so in every reply rather than
                        // parroting the user back (same rule as `other` below).
                        "anthropic" | "openai" => {
                            error!(provider = %entry.provider, error = %e, "model client unavailable — replying with a misconfiguration notice instead of echo");
                            (
                                Arc::new(TaskRunner::new_stub_misconfigured(no_model_notice(
                                    &profile.inner.name,
                                    &format!(
                                        "the {} client could not be built: {e:#}",
                                        entry.provider
                                    ),
                                ))),
                                None,
                                Some(pool.clone()),
                                None,
                            )
                        }
                        // A DELIBERATE `provider: echo` — the test stub. This
                        // arm means the user asked for a parrot, and only that.
                        "echo" => (
                            Arc::new(TaskRunner::new_stub_echo()),
                            None,
                            Some(pool.clone()),
                            None,
                        ),
                        UNRESOLVED_PROVIDER => (
                            Arc::new(TaskRunner::new_stub_misconfigured(no_model_notice(
                                &profile.inner.name,
                                unresolved
                                    .as_deref()
                                    .unwrap_or("model reference did not resolve"),
                            ))),
                            None,
                            Some(pool.clone()),
                            None,
                        ),
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
                                None,
                            )
                        }
                    }
                }
            },
        );
    }

    // Chain and/or routing configured → routing-aware fallback client, behind
    // the same hot-switch seam as the single-model path (`chain_switch_handle`).
    // Reusable per-ref client builder: model_ref -> Arc<dyn LlmClient>.
    // Reuses a fresh registry lookup (resolve_model_entry keys off
    // profile.model_ref; here we resolve an explicit candidate ref) +
    // build_client_from_entry (Step 1). Send + Sync: captures only a cloned
    // Profile and a cloned PathBuf.
    let profile_for_chain = profile.clone();
    let mur_home_for_chain = mur_home.clone();
    let build_one: RefClientBuilder = Arc::new(move |model_ref: &str| {
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
    });
    let (switchable, build_client) =
        chain_switch_handle(profile.inner.clone(), switch_cfg, telemetry, build_one);
    let handle = Arc::new(crate::llm::switchable::ModelSwitchHandle {
        switchable: switchable.clone(),
        build_client,
    });
    let (r, c, p) = build(switchable as Arc<dyn LlmClient>);
    Ok((r, c, p, Some(handle)))
}

/// Turns a registry ref into a concrete client; shared by the chain's own
/// fallback and by `/model`, so both resolve a ref the same way.
pub(crate) type RefClientBuilder =
    Arc<dyn Fn(&str) -> anyhow::Result<Arc<dyn LlmClient>> + Send + Sync>;

/// The hot-switch seam for a chain/routing agent. `/model` on such an agent
/// swaps the PRIMARY and keeps the chain: the chain is the safety net, the
/// primary is the choice. Returns the live slot and a per-ref builder that
/// produces a whole routed client whose profile names `model_ref` as primary,
/// so `resolve_model_refs` keeps deriving `[primary, ...chain]` exactly as at
/// boot, telemetry included.
///
/// Before this, chain agents got no handle at all — and because the global
/// `models.fallback_chain` / `models.smart` settings make EVERY agent a chain
/// agent, `/model` stopped hot-switching anything the moment either was set:
/// each attempt reported `method not found: model/set`, wrote the profile and
/// asked for a restart.
pub(crate) fn chain_switch_handle(
    profile: mur_common::agent::AgentProfile,
    cfg: mur_common::config::ModelSwitchConfig,
    telemetry: Option<tokio::sync::mpsc::Sender<crate::telemetry_writer::Event>>,
    build_one: RefClientBuilder,
) -> (
    Arc<crate::llm::switchable::SwitchableLlmClient>,
    crate::llm::fallback::ClientFactory,
) {
    let validate = build_one.clone();
    let agent = profile.name.clone();
    let make = Arc::new(move |primary: Option<&str>| -> Arc<dyn LlmClient> {
        let mut p = profile.clone();
        if let Some(r) = primary {
            p.model_ref = Some(r.to_string());
        }
        let build_one = build_one.clone();
        let factory: crate::llm::fallback::ClientFactory = Box::new(move |r: &str| build_one(r));
        let mut fb = crate::llm::fallback::FallbackLlmClient::new_routed(
            p,
            cfg.clone(),
            factory,
            cfg.retry.clone(),
        );
        if let Some(tx) = telemetry.clone() {
            fb = fb.with_telemetry(tx, agent.clone());
        }
        Arc::new(fb)
    });
    let switchable = crate::llm::switchable::SwitchableLlmClient::new(make(None));
    let build_client: crate::llm::fallback::ClientFactory = Box::new(move |model_ref: &str| {
        // A ref the registry does not know fails the switch here, where
        // `model/set` keeps the old client, instead of on the next turn, where
        // the chain would quietly answer for a primary that never existed.
        validate(model_ref)?;
        Ok(make(Some(model_ref)))
    });
    (switchable, build_client)
}
