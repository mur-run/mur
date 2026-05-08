pub(crate) mod agent;
pub mod agent_companion;
/// B0 M11.4 — eval-harness JSONL → markdown report aggregator.
pub(crate) mod agent_eval;
pub mod agent_export;
pub mod agent_export_gui;
/// A1 — hooks show [--json] CLI verb.
pub mod agent_hooks;
/// B0 M9.2 — install-time MCP hash + publisher prompt for `mur agent mcp add`.
pub(crate) mod agent_mcp_pin;
pub(crate) mod agent_rekey;
/// C4 — schedule add/list/remove/next CLI verbs.
pub mod agent_schedule;
pub mod agent_voice;
/// Track C5 / M5.1 — webhook receiver config + CLI verbs.
pub(crate) mod agent_webhook;
pub(crate) mod community_cmd;
pub(crate) mod context;
pub(crate) mod conversations_cmd;
pub mod conversations_cost_report;
pub(crate) mod deploy;
pub mod doctor;
pub(crate) mod drafts;
pub(crate) mod evolve_cmd;
pub(crate) mod hook;
pub(crate) mod init;
pub(crate) mod init_local;
pub(crate) mod inject_cmd;
pub(crate) mod learn;
pub(crate) mod misc;
pub(crate) mod model;
pub(crate) mod murmurd;
pub(crate) mod pattern;
pub(crate) mod reindex;
pub(crate) mod server_cmd;
pub(crate) mod session;
pub(crate) mod sync_cmd;
pub(crate) mod system_schedule;
#[allow(dead_code)]
pub(crate) mod var;
pub(crate) mod verify;
pub(crate) mod workflow;

#[cfg(feature = "sources")]
pub(crate) mod source_cmd;

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
