//! Boot-time runtime preparation: telemetry, hooks, and skills.

use std::sync::Arc;

use crate::companion::clock::SystemClock;
use crate::hooks::{HookChain, HookCtx, TelemetryEmitter};
use crate::profile::Profile;
use crate::skills::RuntimeSkills;
use crate::telemetry_writer::{TelemetryWriter, WriterTelemetryEmitter};
use mur_common::config::SkillsConfig;
use mur_common::skill::aggregator::{StatsAggregator, StatsEvent};
use mur_common::telemetry::{
    METHOD_SKILL_EXECUTED, MUR_SKILL_DURATION_MS, MUR_SKILL_MANIFEST_DIGEST, MUR_SKILL_NAME,
    MUR_SKILL_OUTCOME, MUR_SKILL_VERSION,
};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

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
    mur_common::config::MemoryConfig,
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
    let mcp_server_binaries: Vec<std::path::PathBuf> = {
        // Resolve against the same child PATH the spawn uses
        // (`mcp_client::spawn`), so the B0 signature/pin checks (rules 6 & 11)
        // inspect the same binary `Command::new` will exec — including under a
        // Hub-spawned sidecar whose ambient GUI PATH lacks the standard
        // install dirs. A bare `node`/`npx` taken verbatim is a CWD-relative
        // path that doesn't exist, which silently skips both checks.
        // Unresolvable → drop (treated as "uninstalled", matching the
        // soft-fail behaviour in rule 6).
        //
        // Enabled entries only, for the same reason rule 6 filters: a disabled
        // server is never spawned, so it has no business refusing startup —
        // and `mcp disable` has to stay a way out.
        let aug_path = crate::sandbox::search_dirs::mcp_child_path(
            profile.inner.entitlements.processes.spawn.mode,
        );
        profile
            .inner
            .enabled_mcp_servers()
            .iter()
            .filter_map(|s| {
                let prog = s.command.split_whitespace().next().unwrap_or(&s.command);
                mur_common::exec::resolve_command_in(&aug_path, prog).ok()
            })
            .collect()
    };

    // B0 rules 11 + 6: MCP supply-chain admission control. Runs BEFORE the
    // hook chain because it must be able to abort startup — `on_startup` is an
    // observe-only phase that folds hook errors into warnings, which is why
    // these two rules never refused anything until #791.
    crate::hooks::b0::verify_mcp_supply_chain(&mcp_server_binaries, &profile.inner)
        .map_err(|e| anyhow::anyhow!(e))?;

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

    // `mur_home` is a directory; `load_or_default` wants the config FILE.
    // Passing the directory made `read_to_string` fail, which `load_or_default`
    // swallows into `Config::default()` — so every `skills:` setting the user
    // wrote was silently ignored here. Compare the correct call above, which
    // joins "config.yaml".
    let prompt_cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let skills_cfg = prompt_cfg.skills.clone();
    let memory_cfg = prompt_cfg.memory.clone();
    let loaded = crate::skills::drop_forgotten_notes(
        &mur_home,
        &profile.inner.name,
        mur_common::skill::loader::load_all(&mur_home, &profile.inner.name),
    );
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
            SkillRefStatus::CorruptRef { reason } => tracing::warn!(
                skill_ref = %r,
                reason = %reason,
                "profile.yaml holds a corrupted skill ref; the skills it names may well be \
                 installed — installing anything will not help"
            ),
        }
    }
    // The profile denylist, captured once so reloads apply the same gate the
    // boot load did. Cloned out of the profile because a reload can fire long
    // after this borrow ends.
    // The whole profile, not a hand-picked subset: `skill_enabled` reads a
    // denylist AND an add-on group's enabled flag today, and rebuilding a
    // partial `AgentProfile` here would silently stop gating on whatever it
    // learns to read next. Boot-time snapshot, like every other profile read
    // in the runtime — editing profile.yaml still needs a restart.
    let gate_profile = profile.inner.clone();
    let enabled: Box<dyn Fn(&str) -> bool + Send + Sync> =
        Box::new(move |name: &str| gate_profile.skill_enabled(name));
    let loaded: Vec<_> = loaded.into_iter().filter(|s| enabled(&s.name)).collect();
    let runtime_skills =
        Arc::new(RuntimeSkills::build(loaded).reloadable(&mur_home, &profile.inner.name, enabled));

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
        memory_cfg,
    ))
}
