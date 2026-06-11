use anyhow::{Context, Result};
use std::io::Read;

use crate::inject::event::{EventKind, parse_event};
use crate::inject::queue::enqueue;
use crate::retrieve::gate::{GateInputs, Tier as GateTier, evaluate_query_v2};
use crate::retrieve::scoring::score_and_rank_generic;
use crate::retrieve::skill_candidates::load_skill_candidates;
use crate::store::workflow_yaml::WorkflowYamlStore;
use mur_common::skill::stats::LifecycleState;

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

fn is_pre_tool_use(raw: &serde_json::Value) -> bool {
    !matches!(raw.get("tool_response"), Some(v) if !v.is_null())
}

fn is_l2_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "bash" | "multiedit"
    )
}

fn workflow_name_matches_query(query: &str, workflow_names: &[String]) -> bool {
    let query_words: Vec<String> = query
        .to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(str::to_owned)
        .collect();
    workflow_names.iter().any(|name| {
        name.to_ascii_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 4)
            .any(|word| query_words.iter().any(|qw| qw == word))
    })
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

// ── Git-commit auto-index helpers ─────────────────────────────────────────

/// Keywords that trigger a background project reindex.
const INDEX_TRIGGER_COMMANDS: &[&str] = &["git commit", "git push"];

fn should_trigger_index(tool_input: &serde_json::Value) -> bool {
    let command = tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let command_lower = command.to_lowercase();
    INDEX_TRIGGER_COMMANDS
        .iter()
        .any(|trigger| command_lower.contains(trigger))
}

fn spawn_background_index() {
    let mur_bin = std::env::current_exe()
        .ok()
        .unwrap_or_else(|| std::path::PathBuf::from("mur"));

    tracing::info!("git commit detected — spawning background project index");
    if let Err(e) = std::process::Command::new(&mur_bin)
        .args(["project", "index", "--quiet", "--background"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tracing::warn!(error = %e, "failed to spawn background index");
    }
}

pub(crate) async fn cmd_hook_prompt(tool: &str) -> Result<()> {
    let t0 = std::time::Instant::now();
    let raw = read_stdin_json();
    let event = parse_event(raw.clone(), EventKind::Prompt, tool);
    let _ = enqueue(&event);
    let _ = crate::session::ambient::capture(&event);

    // Ensure murmurd is running; respawn silently if heartbeat is stale.
    if !crate::daemon::is_daemon_healthy() {
        crate::daemon::try_respawn_daemon();
    }

    let query = extract_query(&raw).unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(());
    }

    let inputs = GateInputs::default();
    let outcome = evaluate_query_v2(&query, &inputs);
    if outcome.tier == GateTier::Skip {
        return Ok(());
    }

    // Bump to L1 when query overlaps a workflow name — workflow triggers bypass L0 cap
    let effective_tier = if outcome.tier < GateTier::L1 {
        let workflow_names: Vec<String> = WorkflowYamlStore::default_store()
            .and_then(|s| s.list_all())
            .unwrap_or_default()
            .into_iter()
            .map(|w| w.name.clone())
            .collect();
        if workflow_name_matches_query(&query, &workflow_names) {
            GateTier::L1
        } else {
            outcome.tier
        }
    } else {
        outcome.tier
    };

    // Inbox-first path: serve pre-computed context from murmurd if fresh
    if let Some(session_id) = event.session_id.as_deref() {
        let inbox = crate::daemon::inbox_path(session_id);
        if let Some(content) = crate::daemon::read_inbox(&inbox, 300) {
            print!("{content}");
            return Ok(());
        }
    }

    // Degraded-mode / cold-start fallback: synchronous skill retrieval
    let mur_dir = mur_common::trust::mur_home();
    let candidates = load_skill_candidates(&mur_dir.join("skills"), &mur_dir).unwrap_or_default();
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    let scored: Vec<_> = score_and_rank_generic(&query, candidates)
        .into_iter()
        .filter(|s| s.item.stats.lifecycle_state != LifecycleState::Archived)
        .collect();

    let budget = match effective_tier {
        GateTier::L0 => 300,
        GateTier::L1 => 500,
        GateTier::L2 => 2000,
        GateTier::Skip => unreachable!(),
    };

    let output = crate::inject::hook::format_skills_for_injection(&scored, &workflows, budget);

    if !output.is_empty() {
        print!("{output}");
    }

    let duration_ms = t0.elapsed().as_millis() as u64;
    let mut done_event = parse_event(serde_json::json!({}), EventKind::Prompt, tool);
    done_event.duration_ms = Some(duration_ms);
    done_event.session_id = event.session_id.clone();
    done_event.is_duration_record = true;
    let _ = enqueue(&done_event);

    Ok(())
}

pub(crate) async fn cmd_hook_tool(tool: &str) -> Result<()> {
    let t0 = std::time::Instant::now();
    let raw = read_stdin_json();
    let event = parse_event(raw.clone(), EventKind::Tool, tool);
    let _ = enqueue(&event);

    // Check for git commits and trigger background reindex
    if tool == "claude"
        && let Some(ref tool_input) = event.tool_input
        && should_trigger_index(tool_input)
    {
        spawn_background_index();
    }

    // Emit L2 only on PreToolUse for code-editing tools
    if !is_pre_tool_use(&raw) {
        // PostToolUse: ambient-capture the executed tool call (input + exit code).
        let _ = crate::session::ambient::capture(&event);
        return Ok(());
    }
    let tool_called = event.tool_called.as_deref().unwrap_or("");
    if !is_l2_tool(tool_called) {
        return Ok(());
    }

    // Use tool_input as the query hint (file path / bash command gives keyword signals)
    let query_owned: String = event
        .tool_input
        .as_ref()
        .and_then(|v| {
            // Try common string keys in order of specificity
            v.get("file_path")
                .or_else(|| v.get("path"))
                .or_else(|| v.get("command"))
                .and_then(|s| s.as_str())
                .map(str::to_owned)
                .or_else(|| {
                    // Fall back to the whole JSON stringified (gives BM25 tokens)
                    serde_json::to_string(v).ok()
                })
        })
        .unwrap_or_else(|| tool_called.to_owned());
    let query = query_owned.as_str();
    if query.trim().is_empty() {
        return Ok(());
    }

    let mur_dir = mur_common::trust::mur_home();
    let candidates = load_skill_candidates(&mur_dir.join("skills"), &mur_dir).unwrap_or_default();
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    let scored: Vec<_> = score_and_rank_generic(query, candidates)
        .into_iter()
        .filter(|s| s.item.stats.lifecycle_state != LifecycleState::Archived)
        .collect();

    const L2_BUDGET: usize = 2000;
    let output = crate::inject::hook::format_skills_for_injection(&scored, &workflows, L2_BUDGET);

    if !output.is_empty() {
        print!("{output}");
    }

    let duration_ms = t0.elapsed().as_millis() as u64;
    let mut done_event = parse_event(serde_json::json!({}), EventKind::Tool, tool);
    done_event.duration_ms = Some(duration_ms);
    done_event.session_id = event.session_id.clone();
    done_event.is_duration_record = true;
    let _ = enqueue(&done_event);

    Ok(())
}

pub(crate) async fn cmd_hook_stop(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw, EventKind::Stop, tool);
    let _ = enqueue(&event);
    let _ = crate::session::ambient::capture(&event);
    spawn_background_pipeline();
    Ok(())
}

pub(crate) async fn cmd_hook_session_start(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw, EventKind::SessionStart, tool);
    let _ = enqueue(&event);

    // Housekeeping: retention GC (+ harvest scan from W2) runs detached so the
    // hook stays fast.
    let mur_bin = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("mur"));
    let _ = std::process::Command::new(&mur_bin)
        .args(["session", "gc"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    let mur_dir = mur_common::trust::mur_home();
    let candidates = load_skill_candidates(&mur_dir.join("skills"), &mur_dir).unwrap_or_default();

    let project = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

    let index = crate::inject::index::build_from_skills(&candidates, project.as_deref());
    if let Err(e) = crate::inject::index::save(&index) {
        tracing::warn!("capability index save failed (non-fatal): {e}");
    }

    let output = crate::inject::index::format_l0(&index, crate::inject::index::L0_BUDGET_CHARS);

    if !output.is_empty() {
        print!("{output}");
    }

    // §3.8 tier-1: one-line harvest hint (config-gated, zero tokens — counts files only).
    let hint_enabled = crate::store::config::load_config()
        .map(|c| c.harvest.session_start_hint)
        .unwrap_or(true);
    if hint_enabled
        && let Ok(pending) =
            crate::harvest::proposal::pending_in_dir(&crate::harvest::proposal::inbox_dir())
        && !pending.is_empty()
    {
        println!(
            "📥 {} workflow proposal(s) pending — run `mur out` to review.",
            pending.len()
        );
    }
    Ok(())
}

pub(crate) fn cmd_hook_stats() -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let queue_path = home.join(".mur").join("queue").join("events.jsonl");
    let events = crate::inject::queue::read_all_events(&queue_path);
    let stats = crate::inject::stats::compute(&events);
    let output = crate::inject::stats::format_stats(&stats, &queue_path.display().to_string());
    print!("{output}");
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
mod workflow_trigger_tests {
    use super::*;

    #[test]
    fn exact_workflow_name_matches() {
        let names = vec!["deploy-production".to_owned()];
        assert!(workflow_name_matches_query(
            "deploy the production service",
            &names
        ));
    }

    #[test]
    fn partial_workflow_name_matches() {
        let names = vec!["search-bookstore".to_owned()];
        assert!(workflow_name_matches_query(
            "search for latest books",
            &names
        ));
    }

    #[test]
    fn unrelated_query_does_not_match() {
        let names = vec!["deploy-production".to_owned()];
        assert!(!workflow_name_matches_query("fix the lint error", &names));
    }

    #[test]
    fn short_words_are_ignored() {
        let names = vec!["run-ci".to_owned()];
        // "run" (3 chars) and "ci" (2 chars) — both < 4 chars, no match
        assert!(!workflow_name_matches_query("run ci now", &names));
    }

    #[test]
    fn empty_workflow_list_never_matches() {
        assert!(!workflow_name_matches_query(
            "deploy production service",
            &[]
        ));
    }
}

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
