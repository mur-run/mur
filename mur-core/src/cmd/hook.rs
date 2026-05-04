use anyhow::Result;
use std::io::Read;

use crate::inject::event::{EventKind, parse_event};
use crate::inject::queue::enqueue;
use crate::retrieve::gate::{GateInputs, Tier as GateTier, evaluate_query_v2};
use crate::retrieve::scoring::score_and_rank;
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

// ── Internal helpers ──────────────────────────────────────────────────────────

fn read_stdin_json() -> serde_json::Value {
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
    if buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&buf).unwrap_or(serde_json::json!({}))
    }
}

pub(crate) fn extract_query(raw: &serde_json::Value) -> Option<String> {
    raw.get("prompt")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn should_skip(query: Option<&str>) -> bool {
    let q = match query {
        Some(q) if !q.trim().is_empty() => q,
        _ => return true,
    };
    let inputs = GateInputs::default();
    evaluate_query_v2(q, &inputs).tier == GateTier::Skip
}

// ── Command handlers ──────────────────────────────────────────────────────────

pub(crate) async fn cmd_hook_prompt(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw.clone(), EventKind::Prompt, tool);
    let _ = enqueue(&event); // best-effort; never fail the hook on queue write error

    let query = extract_query(&raw).unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(());
    }

    let inputs = GateInputs::default();
    let outcome = evaluate_query_v2(&query, &inputs);
    if outcome.tier == GateTier::Skip {
        return Ok(());
    }

    // Keyword-only quick-path retrieval (no embedding in M1 hot path)
    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    use mur_common::pattern::LifecycleStatus;
    let injected: Vec<_> = score_and_rank(&query, patterns)
        .into_iter()
        .filter(|sp| sp.pattern.lifecycle.status != LifecycleStatus::Archived)
        .map(|sp| sp.pattern)
        .collect();

    let budget = match outcome.tier {
        GateTier::L0 => 300,
        GateTier::L1 => 500,
        GateTier::L2 => 2000,
        GateTier::Skip => unreachable!(),
    };

    let output = crate::inject::hook::format_unified_injection_with_store(
        &injected,
        &workflows,
        budget,
        Some(&yaml_store),
    );

    if !output.is_empty() {
        print!("{output}");
    }
    Ok(())
}

pub(crate) async fn cmd_hook_tool(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw, EventKind::Tool, tool);
    let _ = enqueue(&event);
    Ok(())
}

pub(crate) async fn cmd_hook_stop(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw, EventKind::Stop, tool);
    let _ = enqueue(&event);
    spawn_background_pipeline();
    Ok(())
}

pub(crate) async fn cmd_hook_session_start(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw, EventKind::SessionStart, tool);
    let _ = enqueue(&event);

    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;

    let project = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

    let index = crate::inject::index::build(&patterns, project.as_deref());
    let _ = crate::inject::index::save(&index);

    const L0_BUDGET_CHARS: usize = 2400;
    let output = crate::inject::index::format_l0(&index, L0_BUDGET_CHARS);

    if !output.is_empty() {
        print!("{output}");
    }
    Ok(())
}

pub(crate) fn cmd_hook_stats() -> Result<()> {
    println!("hook stats: not yet implemented (M5)");
    Ok(())
}

// ── Background pipeline (replaces on-stop.sh background block) ───────────────

fn spawn_background_pipeline() {
    let mur_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("mur"));

    // sync is fast; spawn detached so parent exits < 50ms
    let _ = std::process::Command::new(&mur_bin)
        .arg("sync")
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    // evolve + emerge are slow; spawn them sequentially-in-background via a detached child
    // Use separate spawn() calls instead of sh -c to avoid path-with-spaces issues
    let _ = std::process::Command::new(&mur_bin)
        .arg("evolve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    let _ = std::process::Command::new(&mur_bin)
        .arg("emerge")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ack_query_skips() {
        assert!(should_skip(Some("ok")));
        assert!(should_skip(Some("好")));
        assert!(should_skip(Some("thanks")));
    }

    #[test]
    fn empty_query_skips() {
        assert!(should_skip(None));
        assert!(should_skip(Some("")));
        assert!(should_skip(Some("   ")));
    }

    #[test]
    fn coding_query_does_not_skip() {
        assert!(!should_skip(Some(
            "refactor the token budget enforcement to support per-tier caps"
        )));
        assert!(!should_skip(Some(
            "implement retry logic with exponential backoff"
        )));
    }

    #[test]
    fn extract_query_from_claude_raw() {
        let raw = json!({"prompt": "implement error retry", "session_id": "s1"});
        assert_eq!(
            extract_query(&raw).as_deref(),
            Some("implement error retry")
        );
    }

    #[test]
    fn extract_query_missing_returns_none() {
        let raw = json!({"tool_name": "Edit"});
        assert!(extract_query(&raw).is_none());
    }
}
