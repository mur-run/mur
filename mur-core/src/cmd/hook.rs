use anyhow::{Context, Result};
use mur_compress::{AutoCfg, CompressConfig, CompressEngine};
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

/// True iff `value` is already a compressed envelope, i.e. a JSON object
/// whose top-level `"compressed"` key is `true`.
fn is_compressed_envelope(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .and_then(|m| m.get("compressed"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Decide whether the PostToolUse `tool_response` should be auto-compressed
/// and, if so, build the full printable hook-output line. Pure/unit-testable:
/// takes the already-constructed engine and config, does no I/O itself.
///
/// Returns `None` when the gate doesn't fire (feature disabled, no
/// `tool_response`, already a compressed envelope, or compression didn't
/// pay off) — in which case the caller must print nothing.
fn compress_tool_response(
    raw: &serde_json::Value,
    cfg: &AutoCfg,
    engine: &CompressEngine,
) -> Option<String> {
    if !(cfg.enabled && cfg.claude_hook) {
        return None;
    }
    // MUR's own compress tools must never be re-compressed: mur_retrieve's
    // whole purpose is returning the large original, so gating it would loop
    // (retrieve → compress → retrieve → …) and make big entries unrecoverable.
    if raw
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|n| {
            n.ends_with("mur_retrieve")
                || n.ends_with("mur_compress")
                || n.ends_with("mur_compress_stats")
        })
    {
        return None;
    }
    let tool_response = raw.get("tool_response")?;
    if tool_response.is_null() || is_compressed_envelope(tool_response) {
        return None;
    }
    let replacement =
        mur_compress::auto_compress_value(engine, tool_response, None, cfg.min_tokens)?;
    // Emit the replacement with its shape intact. Claude Code validates
    // `updatedToolOutput` against the ORIGINATING tool's output schema, so
    // stringifying an object replacement (Edit/Write/Bash/Agent all hand us
    // objects, and `auto_compress_value` deliberately preserves that shape by
    // compressing only the largest string/array field) fails validation with
    // "expected object, received string" and the compression is discarded.
    let line = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": replacement,
        }
    });
    serde_json::to_string(&line).ok()
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

// ── Dev-discipline hub suppression (spec 2026-07-23) ──────────────────────────

/// Skill name of the dev-discipline hub.
pub(crate) const MUR_DEV_HUB_NAME: &str = "mur-dev";

/// Directory-name marker identifying a superpowers plugin install.
const SUPERPOWERS_MARKER: &str = "superpowers";

/// True if a Claude-Code superpowers plugin install exists under
/// `<user_home>/.claude/plugins/{cache,repos,marketplaces}` (dir whose name
/// contains "superpowers", checked one and two levels deep).
fn superpowers_plugin_present(user_home: &std::path::Path) -> bool {
    let has_marker = |p: &std::path::Path| {
        p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.to_ascii_lowercase().contains(SUPERPOWERS_MARKER))
    };
    for sub in ["plugins/cache", "plugins/repos", "plugins/marketplaces"] {
        let base = user_home.join(".claude").join(sub);
        let Ok(level1) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in level1.flatten() {
            let p = entry.path();
            if has_marker(&p) {
                return true;
            }
            if p.is_dir()
                && let Ok(level2) = std::fs::read_dir(&p)
            {
                for e2 in level2.flatten() {
                    if has_marker(&e2.path()) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Whether the `mur-dev` hub must be dropped from the CLI learning index.
/// Never affects runtime (agent) injection — this is only called on the
/// session-start hook path.
fn dev_hub_suppressed(
    idx_cfg: mur_common::config::DevDisciplineIndex,
    user_home: &std::path::Path,
) -> bool {
    use mur_common::config::DevDisciplineIndex as D;
    match idx_cfg {
        D::Always => false,
        D::Never => true,
        D::Auto => superpowers_plugin_present(user_home),
    }
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
    let mut candidates =
        load_skill_candidates(&mur_dir.join("skills"), &mur_dir).unwrap_or_default();
    // Scope filter: fail-closed for fleet/project-tagged skills (only inject them
    // when the runtime's active scope matches). User/Enterprise always pass.
    crate::retrieve::skill_candidates::filter_by_scope(
        &mut candidates,
        &crate::retrieve::skill_candidates::ActiveScope::detect(),
    );
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

        // Best-effort auto-compression of the tool_response, gated on
        // auto.enabled && auto.claude_hook. Any failure to build the engine
        // is silently skipped — identical behavior to today.
        if let Ok(home) = crate::cmd::agent::resolve_mur_home() {
            let cfg = CompressConfig::load(&home);
            let auto_cfg = cfg.auto.clone();
            if let Ok(engine) = CompressEngine::new(home.join("compress"), cfg)
                && let Some(line) = compress_tool_response(&raw, &auto_cfg, &engine)
            {
                println!("{line}");
            }
        }

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
    let mut candidates =
        load_skill_candidates(&mur_dir.join("skills"), &mur_dir).unwrap_or_default();
    // Scope filter: fail-closed for fleet/project-tagged skills (only inject them
    // when the runtime's active scope matches). User/Enterprise always pass.
    crate::retrieve::skill_candidates::filter_by_scope(
        &mut candidates,
        &crate::retrieve::skill_candidates::ActiveScope::detect(),
    );
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
    let mut candidates =
        load_skill_candidates(&mur_dir.join("skills"), &mur_dir).unwrap_or_default();
    // Scope filter: fail-closed for fleet/project-tagged skills (only inject them
    // when the runtime's active scope matches). User/Enterprise always pass.
    crate::retrieve::skill_candidates::filter_by_scope(
        &mut candidates,
        &crate::retrieve::skill_candidates::ActiveScope::detect(),
    );

    // Display label for the skill index (last path component) — distinct from
    // ActiveScope.project (the repo-root id used for scope matching).
    let project_label = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

    let mut index = crate::inject::index::build_from_skills(&candidates, project_label.as_deref());
    if let Err(e) = crate::inject::index::save(&index) {
        tracing::warn!("capability index save failed (non-fatal): {e}");
    }

    // Drop the mur-dev hub from the CLI index when superpowers is present (or
    // config forces it). Applies only to printed output — the saved index above
    // keeps every entry, and runtime (agent) injection is unaffected.
    let idx_cfg = crate::store::config::load_config()
        .map(|c| c.skills.dev_discipline_index)
        .unwrap_or_default();
    if let Some(user_home) = dirs::home_dir()
        && dev_hub_suppressed(idx_cfg, &user_home)
    {
        index.entries.retain(|e| e.name != MUR_DEV_HUB_NAME);
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
            "📥 {} workflow proposal(s) pending — run `mur session out` to review.",
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
    // (`mur evolve` / `mur emerge` spawns removed — those subcommands never
    // existed; the spawns failed silently on every Stop hook. v2 P1a cleanup.)
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

    #[test]
    fn superpowers_detection_and_suppression_matrix() {
        use mur_common::config::DevDisciplineIndex as D;
        let home = tempfile::tempdir().unwrap();
        // No plugin dirs at all → not present.
        assert!(!super::superpowers_plugin_present(home.path()));
        assert!(!super::dev_hub_suppressed(D::Auto, home.path()));
        // Marker dir two levels under plugins/cache → present.
        let plug = home
            .path()
            .join(".claude/plugins/cache/claude-plugins-official/superpowers");
        std::fs::create_dir_all(&plug).unwrap();
        assert!(super::superpowers_plugin_present(home.path()));
        assert!(super::dev_hub_suppressed(D::Auto, home.path()));
        // Config overrides beat detection.
        assert!(!super::dev_hub_suppressed(D::Always, home.path()));
        assert!(super::dev_hub_suppressed(D::Never, home.path()));
        // `Never` suppresses even without a plugin present.
        let empty = tempfile::tempdir().unwrap();
        assert!(super::dev_hub_suppressed(D::Never, empty.path()));
    }
}

#[cfg(test)]
mod compress_tool_response_tests {
    use super::*;
    use mur_compress::{AutoCfg, CompressConfig, CompressEngine};
    use serde_json::json;

    /// Mirrors the `engine()` test helper in `mur-compress/src/auto.rs`: a
    /// throwaway store dir plus a default-config `CompressEngine`.
    fn engine() -> (tempfile::TempDir, CompressEngine) {
        let dir = tempfile::tempdir().unwrap();
        let eng = CompressEngine::new(dir.path().to_path_buf(), CompressConfig::default()).unwrap();
        (dir, eng)
    }

    fn auto_cfg() -> AutoCfg {
        AutoCfg {
            enabled: true,
            claude_hook: true,
            ..AutoCfg::default()
        }
    }

    /// Large, repetitive JSON array well above the `min_tokens` gate — stands
    /// in for an oversized MCP tool result.
    fn big_tool_response() -> serde_json::Value {
        let items: Vec<String> = (0..4000)
            .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\",\"value\":{}}}", i * 7))
            .collect();
        serde_json::from_str(&format!("[{}]", items.join(","))).unwrap()
    }

    #[test]
    fn oversized_mcp_tool_result_compresses_and_prints_line() {
        let (_dir, eng) = engine();
        let cfg = auto_cfg();
        let raw = json!({
            "tool_name": "mcp__desktop-commander__list_processes",
            "tool_response": big_tool_response(),
        });

        let line = compress_tool_response(&raw, &cfg, &eng).expect("gate should fire");
        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("line must be valid JSON");

        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            json!("PostToolUse")
        );
        let updated = &parsed["hookSpecificOutput"]["updatedToolOutput"];
        // An array response has no tool-declared shape to preserve (this path
        // serves MCP list results), so it keeps the object envelope. What must
        // NOT happen is the envelope arriving JSON-stringified — Claude Code
        // validates against the originating tool's schema and discards it.
        assert!(
            updated.is_object(),
            "envelope must not be stringified; got: {updated}"
        );
        assert_eq!(updated["compressed"], json!(true));
        assert!(
            updated["hash"].as_str().is_some_and(|h| !h.is_empty()),
            "offloaded result must carry a hash: {updated}"
        );
        assert!(
            updated["note"]
                .as_str()
                .is_some_and(|n| n.contains("mur_retrieve")),
            "expected retrieval marker in note: {updated}"
        );
    }

    #[test]
    fn object_tool_response_keeps_object_shape() {
        // Claude Code validates `updatedToolOutput` against the originating
        // tool's output schema. Edit/Write/Bash/Agent all hand the hook an
        // object, so a stringified replacement is rejected with "expected
        // object, received string" and the compression is silently dropped.
        let (_dir, eng) = engine();
        let cfg = auto_cfg();
        let raw = json!({
            "tool_name": "Bash",
            "tool_response": {
                "stdout": big_tool_response().to_string(),
                "stderr": "",
                "interrupted": false,
            },
        });

        let line = compress_tool_response(&raw, &cfg, &eng).expect("gate should fire");
        let parsed: serde_json::Value =
            serde_json::from_str(&line).expect("line must be valid JSON");
        let updated = &parsed["hookSpecificOutput"]["updatedToolOutput"];

        assert!(
            updated.is_object(),
            "must stay an object or Claude Code discards it; got: {updated}"
        );
        assert_eq!(updated["interrupted"], json!(false), "siblings preserved");
        assert_eq!(updated["stderr"], json!(""), "siblings preserved");
        // `stdout` was declared a string by the tool, so the compressed form
        // must still be a string — the retrieval hash is inlined as text
        // rather than swapped in as the object envelope.
        let stdout = updated["stdout"]
            .as_str()
            .expect("stdout must stay a string, not become the object envelope");
        assert!(
            stdout.contains("mur_retrieve"),
            "compressed stdout should carry the retrieval marker: {stdout}"
        );
    }

    #[test]
    fn small_tool_response_below_floor_is_none() {
        let (_dir, eng) = engine();
        let cfg = auto_cfg();
        let raw = json!({
            "tool_name": "mcp__desktop-commander__list_processes",
            "tool_response": "tiny output",
        });

        assert!(compress_tool_response(&raw, &cfg, &eng).is_none());
    }

    #[test]
    fn already_compressed_envelope_is_none() {
        let (_dir, eng) = engine();
        let cfg = auto_cfg();
        let raw = json!({
            "tool_name": "mcp__desktop-commander__list_processes",
            "tool_response": {
                "compressed": true,
                "content": "already compressed",
                "hash": "deadbeef",
                "original_tokens": 9000,
                "compressed_tokens": 12,
                "note": "Large output compressed; original stored.",
            },
        });

        assert!(compress_tool_response(&raw, &cfg, &eng).is_none());
    }

    #[test]
    fn mur_own_compress_tools_are_exempt() {
        let (_dir, eng) = engine();
        let cfg = auto_cfg();
        let big: Vec<_> = (0..2000)
            .map(|i| json!({"idx": i, "data": "x".repeat(40)}))
            .collect();
        for name in [
            "mcp__mur__mur_retrieve",
            "mcp__mur__mur_compress",
            "mcp__mur__mur_compress_stats",
        ] {
            let raw = json!({"tool_name": name, "tool_response": big});
            assert!(
                compress_tool_response(&raw, &cfg, &eng).is_none(),
                "{name} must never be re-compressed"
            );
        }
    }
}
