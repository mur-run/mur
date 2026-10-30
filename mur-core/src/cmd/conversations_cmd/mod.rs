//! CLI handlers for conversations archive commands.
//! See spec §6.

use anyhow::Result;
use chrono::NaiveDate;
use mur_common::Source;
use tracing::info;

use crate::conversations;

mod ask;
mod backends;
mod doctor;
mod preflight;
mod reindex;

// `pub use` re-exports for the public CLI dispatch API. The lib crate doesn't
// reference these names internally; consumers (main.rs, dispatch.rs) reach
// them via `crate::cmd::conversations_cmd::cmd_conversations_*`. Rustc still
// flags them as `unused_imports` under the lib+bin compilation split —
// silenced here (same idiom as `cmd/agent/mod.rs`).
#[allow(unused_imports)]
pub use ask::{AskArgs, CompactArgs, cmd_ask, cmd_conversations_compact};
#[allow(unused_imports)]
pub use doctor::cmd_conversations_doctor;
#[allow(unused_imports)]
pub use preflight::cmd_conversations_preflight;
#[allow(unused_imports)]
pub use reindex::{ReindexArgs, cmd_conversations_reindex};
// Only the unit tests reach this helper through `use super::*`.
#[cfg(test)]
use ask::resolve_summarize;

pub fn cmd_chat_list(since: Option<String>, src: Option<String>) -> Result<()> {
    let since_date = since
        .as_deref()
        .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        .transpose()?;
    let sources: Vec<Source> = src.as_deref().map(parse_sources).unwrap_or_default();
    let days = conversations::retrieve::list_days(since_date, None, &sources, None)?;
    if days.is_empty() {
        println!("(no conversations)");
        return Ok(());
    }
    for d in days {
        let src_tags: Vec<String> = d.sources.iter().map(|s| s.file_prefix().into()).collect();
        let summary = if d.summary_exists { "✓" } else { "·" };
        println!(
            "{}  {}  {:>4} msgs  [{}]",
            d.date,
            summary,
            d.msg_count,
            src_tags.join(",")
        );
    }
    Ok(())
}

pub fn cmd_chat_show(date: String) -> Result<()> {
    let d = NaiveDate::parse_from_str(&date, "%Y-%m-%d")?;
    if let Some(summary) = conversations::retrieve::show_summary(d, None)? {
        println!("{summary}");
        return Ok(());
    }
    println!("# {d} (no summary; showing raw)\n");
    for m in conversations::retrieve::show_day(d, None)? {
        let text = match &m.content {
            mur_common::Content::Text { value } => value.clone(),
            mur_common::Content::ToolRef { desc, bytes, .. } => {
                format!("[tool_ref: {desc} ({bytes}B)]")
            }
            mur_common::Content::ImageRef { desc, .. } => format!("[image_ref: {desc}]"),
        };
        println!(
            "[{}] {}/{}: {}",
            m.ts.format("%H:%M:%S"),
            m.src.file_prefix(),
            m.conv,
            text
        );
    }
    Ok(())
}

pub fn cmd_chat_raw(date: String, conv: String) -> Result<()> {
    let d = NaiveDate::parse_from_str(&date, "%Y-%m-%d")?;
    let ts = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let dir = conversations::paths::raw_dir_for(ts, None);
    if !dir.exists() {
        println!("(no raw for {d})");
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().contains(&conv) {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())?;
        print!("{content}");
    }
    Ok(())
}

pub async fn cmd_chat_search(query: String, limit: usize, src: Option<String>) -> Result<()> {
    let source_filter = src
        .as_deref()
        .and_then(|s| parse_sources(s).into_iter().next());
    let cfg = crate::store::config::load_config().unwrap_or_default();
    let embed_cfg = crate::store::embedding::EmbeddingConfig::from_config(&cfg);
    let embed = crate::store::embedding::embed(&query, &embed_cfg).await?;
    let hits = conversations::retrieve::search(&query, embed, limit, source_filter, None).await?;
    if hits.is_empty() {
        println!("(no matches)");
        return Ok(());
    }
    for h in hits {
        let when = chrono::DateTime::<chrono::Utc>::from_timestamp(h.ts, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        println!(
            "[{:.2}] {} {}/{}: {}",
            h.score,
            when,
            h.source.file_prefix(),
            h.conv_id,
            truncate(&h.snippet, 120)
        );
    }
    Ok(())
}

pub async fn cmd_conversations_pull() -> Result<()> {
    info!("conversations pull: scanning all poll-based ingesters");
    let mut pipeline = conversations::ingest::pipeline::Pipeline::new(None)?;

    for ws in conversations::ingest::cursor::list_cursor_workspaces() {
        if let Ok(msgs) = conversations::ingest::cursor::scan_workspace(&ws)
            && !msgs.is_empty()
        {
            let r = pipeline.run(msgs)?;
            println!(
                "cursor {}: {} accepted, {} rejected, {} deduped",
                ws.file_name().unwrap().to_string_lossy(),
                r.accepted,
                r.rejected,
                r.deduped
            );
        }
    }

    for path in conversations::ingest::gemini::list_gemini_chats() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let id = path.file_stem().unwrap().to_string_lossy().to_string();
        if let Ok(msgs) = conversations::ingest::gemini::parse_gemini_chat(&v, &id)
            && !msgs.is_empty()
        {
            let r = pipeline.run(msgs)?;
            println!("gemini {}: {} accepted", id, r.accepted);
        }
    }

    let watched = read_aider_watched();
    for hist in conversations::ingest::aider::find_aider_histories(&watched) {
        let Ok(md) = std::fs::read_to_string(&hist) else {
            continue;
        };
        let id = hist
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "aider".into());
        if let Ok(msgs) = conversations::ingest::aider::parse_aider_md(&md, &id)
            && !msgs.is_empty()
        {
            let r = pipeline.run(msgs)?;
            println!("aider {}: {} accepted", id, r.accepted);
        }
    }

    Ok(())
}

pub async fn cmd_conversations_cleanup() -> Result<()> {
    let days = conversations::retention::retention_days_from_config();
    let r = conversations::retention::cleanup(chrono::Utc::now(), days, None)?;
    println!(
        "Scanned {} dirs, deleted {}, {} KB freed, {} kept (no summary), {} errored",
        r.dirs_scanned,
        r.dirs_deleted,
        r.bytes_freed / 1024,
        r.dirs_skipped_no_summary,
        r.dirs_errored
    );
    Ok(())
}

/// Collect the unique BackendConfigs across the six conversations call sites
/// (compact.{extractive, abstractive}, ask.{backend, rewriter_backend},
/// rollup.{extractive, abstractive}), dedup by (provider, model, endpoint) so
/// the same provider+model+endpoint isn't probed twice.
fn collect_backend_configs(
    cfg: &mur_common::config::Config,
) -> Vec<mur_common::config::BackendConfig> {
    let mut backends = vec![
        cfg.conversations
            .compact
            .effective_extractive_backend(&cfg.llm),
        cfg.conversations
            .compact
            .effective_abstractive_backend(&cfg.llm),
        cfg.conversations.ask.effective_backend(&cfg.llm),
        cfg.conversations.ask.effective_rewriter_backend(&cfg.llm),
        cfg.conversations
            .rollup
            .effective_extractive_backend(&cfg.llm),
        cfg.conversations
            .rollup
            .effective_abstractive_backend(&cfg.llm),
    ];
    // NOTE: the dedup key includes `endpoint` on purpose. Two stages can route
    // the same model to two different hosts; keying on (provider, model) alone
    // silently drops one of the endpoints from the probe set (fix round 1,
    // finding 1).
    backends.sort_by(|a, b| {
        (&a.provider, &a.model, &a.endpoint).cmp(&(&b.provider, &b.model, &b.endpoint))
    });
    backends.dedup_by(|a, b| {
        a.provider == b.provider && a.model == b.model && a.endpoint == b.endpoint
    });
    backends
}

/// One Ollama endpoint actually used by the conversations pipeline, and the
/// distinct, non-empty models routed to it.
#[derive(Debug)]
struct OllamaEndpointGroup {
    endpoint: String,
    /// Sorted, deduped, non-empty model names routed to this endpoint.
    models: Vec<String>,
}

/// Groups the Ollama-routed backends by endpoint so each distinct endpoint is
/// probed and validated on its own.
///
/// Before this helper existed, `doctor`/`preflight` each probed
/// `ollama_backends[0].endpoint` — one arbitrarily-chosen endpoint — and
/// checked every Ollama-routed model against it. A user with `ask.backend` on
/// `box.local:11434` and `compact.extractive_backend` on `localhost:11434`
/// got the second stage's model reported missing against a host that was
/// never queried (fix round 1, finding 1). Grouping by endpoint here means a
/// model is only ever validated against the endpoint it is actually routed to.
fn group_ollama_backends_by_endpoint(
    backends: &[mur_common::config::BackendConfig],
) -> Vec<OllamaEndpointGroup> {
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for b in backends.iter().filter(|b| b.provider == "ollama") {
        if b.model.is_empty() {
            continue;
        }
        let endpoint = b
            .endpoint
            .clone()
            .unwrap_or_else(|| mur_common::config::DEFAULT_OLLAMA_ENDPOINT.to_string());
        grouped.entry(endpoint).or_default().push(b.model.clone());
    }
    grouped
        .into_iter()
        .map(|(endpoint, mut models)| {
            models.sort();
            models.dedup();
            OllamaEndpointGroup { endpoint, models }
        })
        .collect()
}

/// Outcome of probing one Ollama endpoint's `/api/tags`.
enum OllamaProbeOutcome {
    /// Reachable; carries the installed model names.
    Reachable(Vec<String>),
    /// Not reachable; carries a human-readable reason ("returned 500", "timed
    /// out (2s)", …).
    Unreachable(String),
}

/// Probes one endpoint's `/api/tags`. Shared by `doctor` (reachability only)
/// and `preflight` (reachability + per-model pull check) — see finding 2 of
/// fix round 1: these were near-duplicated derive-backends → pick-endpoint →
/// probe blocks.
async fn probe_ollama_tags(endpoint: &str, timeout: std::time::Duration) -> OllamaProbeOutcome {
    let url = format!("{}/api/tags", endpoint.trim_end_matches('/'));
    match tokio::time::timeout(timeout, reqwest::get(&url)).await {
        Ok(Ok(resp)) if resp.status().is_success() => {
            let installed = resp
                .text()
                .await
                .ok()
                .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
                .and_then(|v| v.get("models").cloned())
                .and_then(|m| m.as_array().cloned())
                .map(|arr| {
                    arr.into_iter()
                        .filter_map(|e| e.get("name").and_then(|n| n.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            OllamaProbeOutcome::Reachable(installed)
        }
        Ok(Ok(resp)) => OllamaProbeOutcome::Unreachable(format!("returned {}", resp.status())),
        Ok(Err(e)) => OllamaProbeOutcome::Unreachable(format!("unreachable: {e}")),
        Err(_) => OllamaProbeOutcome::Unreachable(format!("timed out ({}s)", timeout.as_secs())),
    }
}

/// BP2 amendment: dry-run by default; `run=true` means actually migrate.
/// BP3 amendment: `resume` and `discard_staging` handle interrupted migrations.
pub async fn cmd_conversations_migrate(
    run: bool,
    resume: bool,
    discard_staging: bool,
) -> Result<()> {
    use crate::conversations::migrate;
    if discard_staging {
        migrate::discard_staging(None).await?;
        println!("staging discarded");
        return Ok(());
    }
    if resume {
        let report = migrate::resume(None).await?;
        println!(
            "resumed: {} messages, {} audit entries, {}ms",
            report.messages_migrated, report.audit_entries_replayed, report.duration_ms
        );
        return Ok(());
    }
    if !run {
        // BP2: dry-run by default
        let plan = migrate::dry_run(None)?;
        println!("{}", migrate::render_plan(&plan));
        return Ok(());
    }
    // run=true: actually migrate (Task 19 fills in run())
    let report = migrate::run(None).await?;
    println!(
        "Migrated {} messages, replayed {} audit entries in {}ms",
        report.messages_migrated, report.audit_entries_replayed, report.duration_ms
    );
    Ok(())
}

pub async fn cmd_conversations_rollback() -> Result<()> {
    let report = crate::conversations::migrate::rollback(None).await?;
    println!(
        "Rolled back {} messages in {}ms",
        report.messages_migrated, report.duration_ms
    );
    Ok(())
}

pub struct RollupArgs {
    pub week: Option<String>,
    pub month: Option<String>,
    pub all_missing: bool,
    pub force: bool,
    /// Phase 3.2.1: Intentionally unused; retained for backward compatibility.
    /// The default (force=false) already triggers sha-based idempotency checks.
    #[allow(dead_code)]
    pub if_stale: bool,
    pub max_weeks: Option<u32>,
    pub max_months: Option<u32>,
}

pub async fn cmd_conversations_rollup(args: RollupArgs) -> Result<()> {
    use crate::conversations::summarize::rollup::{
        RollupKinds, rollup_missing, rollup_month, rollup_week,
    };

    let config = crate::store::config::load_config().unwrap_or_default();
    let rollup_cfg = config.conversations.rollup.clone();

    if let Some(w) = args.week {
        // Phase 3.2.1: --if-stale is a no-op; the default (force=false)
        // already triggers the sha-based idempotency check inside
        // rollup_week. Flag retained for backward-compat with scripts.
        let force = args.force;
        let r = rollup_week(&w, force, &rollup_cfg, &config.llm, None).await?;
        println!("{}: {:?} ({}ms)", r.window, r.outcome, r.duration_ms);
        return Ok(());
    }
    if let Some(m) = args.month {
        // Phase 3.2.1: --if-stale is a no-op; the default (force=false)
        // already triggers the sha-based idempotency check inside
        // rollup_month. Flag retained for backward-compat with scripts.
        let force = args.force;
        let r = rollup_month(&m, force, &rollup_cfg, &config.llm, None).await?;
        println!("{}: {:?} ({}ms)", r.window, r.outcome, r.duration_ms);
        return Ok(());
    }
    if args.all_missing {
        let sweep = rollup_missing(
            &rollup_cfg,
            &config.llm,
            RollupKinds::All,
            args.max_weeks,
            args.max_months,
            None,
        )
        .await?;
        for r in &sweep.reports {
            println!("  {} {:?} ({}ms)", r.window, r.outcome, r.duration_ms);
        }
        println!(
            "rolled up: {} week ok / {} week err / {} week skipped; {} month ok / {} month err / {} month skipped",
            sweep.week_ok,
            sweep.week_err,
            sweep.week_skipped,
            sweep.month_ok,
            sweep.month_err,
            sweep.month_skipped,
        );
        return Ok(());
    }
    anyhow::bail!("supply --week, --month, or --all-missing");
}

fn parse_sources(s: &str) -> Vec<Source> {
    s.split(',')
        .filter_map(|p| match p.trim() {
            "cc" | "claude-code" => Some(Source::ClaudeCode),
            "cursor" => Some(Source::Cursor),
            "gemini" => Some(Source::Gemini),
            "aider" => Some(Source::Aider),
            "slack" => Some(Source::Slack),
            "telegram" | "tg" => Some(Source::Telegram),
            "discord" => Some(Source::Discord),
            "commander" => Some(Source::CommanderEngine),
            _ => None,
        })
        .collect()
}

fn read_aider_watched() -> Vec<std::path::PathBuf> {
    let Some(mur_dir) = mur_common::home::try_mur_home() else {
        return Vec::new();
    };
    let cfg = mur_dir.join("config.yaml");
    let Ok(text) = std::fs::read_to_string(&cfg) else {
        return Vec::new();
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return Vec::new();
    };
    doc.get("conversations")
        .and_then(|c| c.get("sources"))
        .and_then(|s| s.get("aider"))
        .and_then(|a| a.get("watched_dirs"))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(|s| std::path::PathBuf::from(shellexpand::tilde(s).to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= max {
        s.to_string()
    } else {
        format!("{}…", c.iter().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests;
