pub mod agent;
#[allow(unused_imports)]
pub use agent::resolve_mur_home;
pub mod agent_companion;
/// B0 M11.4 — eval-harness JSONL → markdown report aggregator.
pub(crate) mod agent_eval;
pub mod agent_export;
pub mod agent_export_gui;
pub(crate) mod agent_history;
/// A1 — hooks show [--json] CLI verb.
pub mod agent_hooks;
/// B0 M9.2 — install-time MCP hash + publisher prompt for `mur agent mcp add`.
pub(crate) mod agent_mcp_deep_audit;
pub(crate) mod agent_mcp_pin;
pub(crate) mod agent_mcp_vendor;
pub mod agent_pair;
pub(crate) mod agent_propagate;
pub(crate) mod agent_rekey;
/// C4 — schedule add/list/remove/next CLI verbs.
pub mod agent_schedule;
pub mod agent_voice;
/// Track C5 / M5.1 — webhook receiver config + CLI verbs.
pub(crate) mod agent_webhook;
pub mod capability;
pub mod channel;
pub mod commander;
pub mod compress;
pub mod context;
pub(crate) mod conversations_cmd;
pub mod conversations_cost_report;
pub mod deep_research;
pub(crate) mod deploy;
pub mod deps;
pub mod doctor;
pub(crate) mod drafts;
pub(crate) mod eval;
pub mod fleet;
#[allow(dead_code)]
pub mod fleet_sync;
pub(crate) mod hook;
pub(crate) mod init;
pub(crate) mod init_daemon;
pub(crate) mod init_local;
pub(crate) mod inject_cmd;
pub(crate) mod internals;
pub(crate) mod learn;
pub mod media;
pub(crate) mod migrate_patterns;
pub(crate) mod misc;
pub mod model;
pub(crate) mod murmurd;
pub mod notes_cmd;
pub(crate) mod official;
pub mod project;
pub(crate) mod reindex;
pub(crate) mod search;
pub(crate) mod server_cmd;
pub(crate) mod session;
pub mod skill_archive;
pub mod skill_cmd;
pub mod skill_consolidate;
pub mod skill_credit;
pub mod skill_curate;
pub mod skill_deps;
pub mod skill_doctor;
pub mod skill_evolve;
pub mod skill_generate;
pub mod skill_install;
pub mod skill_intent;
pub(crate) mod skill_publish;
pub mod skill_recombine;
pub mod skill_registry;
// wired by Task A2 CLI (mur skill registry-index)
#[allow(dead_code)]
pub mod skill_registry_index;
pub mod skill_reindex_vec;
#[allow(dead_code)]
pub mod skill_resolver;
pub mod skill_stats;
pub mod skill_suggest;
pub mod skill_sweep;
pub mod skill_upgrade;
pub mod skill_upgrade_cmd;
pub(crate) mod sleep;
pub(crate) mod sync_cmd;
pub(crate) mod system_schedule;
pub(crate) mod team_cmd;
pub(crate) mod update;
#[allow(dead_code)]
pub(crate) mod var;
pub(crate) mod verify;
pub mod workflow;
pub mod workflow_delete;

#[cfg(feature = "sources")]
pub(crate) mod source_cmd;

#[allow(dead_code)]
pub(crate) fn read_multiline() -> anyhow::Result<String> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        lines.push(line);
    }
    Ok(lines.join("").trim_end().to_string())
}
