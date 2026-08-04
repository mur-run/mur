//! CLI handlers for conversations archive commands.
//! See spec §6.

use anyhow::Result;
use chrono::NaiveDate;
use mur_common::Source;
use tracing::info;

use crate::conversations;

mod backends;
mod doctor;
mod preflight;

// `pub use` re-exports for the public CLI dispatch API. The lib crate doesn't
// reference these names internally; consumers (main.rs, dispatch.rs) reach
// them via `crate::cmd::conversations_cmd::cmd_conversations_*`. Rustc still
// flags them as `unused_imports` under the lib+bin compilation split —
// silenced here (same idiom as `cmd/agent/mod.rs`).
#[allow(unused_imports)]
pub use doctor::cmd_conversations_doctor;
#[allow(unused_imports)]
pub use preflight::cmd_conversations_preflight;

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

pub struct ReindexArgs {
    pub raw_only: bool,
    pub spans_only: bool,
    pub rollups_only: bool,
}

pub async fn cmd_conversations_reindex(args: ReindexArgs) -> Result<()> {
    use crate::conversations::{paths, store, summarize};

    let mut raw_msgs = 0u64;
    let mut span_rows = 0u64;

    // Raw rebuild (layer=0) — Phase 1 behavior.
    if !args.spans_only && !args.rollups_only {
        let days = store::list_raw_dirs(None).unwrap_or_default();
        let dims: i32 = {
            let cfg = crate::store::config::load_config().unwrap_or_default();
            crate::store::embedding::EmbeddingConfig::from_config(&cfg).dimensions as i32
        };
        let mut idx = crate::conversations::index::ConversationIndex::open(dims, None).await?;
        for (date, _) in days {
            let msgs = store::read_day(date, None)?;
            if msgs.is_empty() {
                continue;
            }
            let embed_cfg = {
                let cfg = crate::store::config::load_config().unwrap_or_default();
                crate::store::embedding::EmbeddingConfig::from_config(&cfg)
            };
            let texts: Vec<String> = msgs
                .iter()
                .map(|m| m.content.as_text().to_owned())
                .collect();
            let vecs = if let Some(mode) = crate::conversations::ollama::mock_mode() {
                texts
                    .iter()
                    .map(|t| {
                        crate::conversations::ollama::mock_embed_vector(t, mode, dims as usize)
                    })
                    .collect::<Vec<_>>()
            } else {
                crate::store::embedding::embed_batch(&texts, &embed_cfg)
                    .await
                    .unwrap_or_else(|_| texts.iter().map(|_| vec![0.0; dims as usize]).collect())
            };
            let entries: Vec<_> = msgs.into_iter().zip(vecs).collect();
            idx.upsert(&entries).await?;
            raw_msgs += entries.len() as u64;
        }
        println!("reindexed raw: {raw_msgs} messages");
    }

    // Span rebuild (layer=2) — Phase 3.1.
    if !args.raw_only && !args.rollups_only {
        let dims: i32 = {
            let cfg = crate::store::config::load_config().unwrap_or_default();
            crate::store::embedding::EmbeddingConfig::from_config(&cfg).dimensions as i32
        };
        let mut idx = crate::conversations::index::ConversationIndex::open(dims, None).await?;
        let summary_dir = paths::conversations_root(None).join("summary");
        if summary_dir.exists() {
            for entry in std::fs::read_dir(&summary_dir)? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Ok(date) = chrono::NaiveDate::parse_from_str(stem, "%Y-%m-%d") else {
                    continue;
                };
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                let Ok(parsed) = summarize::parse_summary(&body) else {
                    continue;
                };
                if parsed.extractive.is_empty() {
                    continue;
                }
                let texts: Vec<String> = parsed.extractive.iter().map(|s| s.text.clone()).collect();
                let vecs: Vec<Vec<f32>> = if let Some(mode) =
                    crate::conversations::ollama::mock_mode()
                {
                    texts
                        .iter()
                        .map(|t| {
                            crate::conversations::ollama::mock_embed_vector(t, mode, dims as usize)
                        })
                        .collect()
                } else {
                    let embed_cfg = {
                        let cfg = crate::store::config::load_config().unwrap_or_default();
                        crate::store::embedding::EmbeddingConfig::from_config(&cfg)
                    };
                    crate::store::embedding::embed_batch(&texts, &embed_cfg)
                        .await
                        .unwrap_or_else(|_| {
                            texts.iter().map(|_| vec![0.0; dims as usize]).collect()
                        })
                };

                use chrono::TimeZone;
                let span_ts = chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap());
                let mut batch: Vec<(mur_common::Message, Vec<f32>, i8)> =
                    Vec::with_capacity(parsed.extractive.len());
                for (span, vec) in parsed.extractive.iter().zip(vecs) {
                    let Some(src_enum) = mur_common::Source::from_prefix(&span.src) else {
                        tracing::warn!(
                            "unknown source prefix '{}' in {}; skipping span",
                            span.src,
                            path.display()
                        );
                        continue;
                    };
                    let msg = mur_common::Message {
                        v: 1,
                        ts: span_ts,
                        src: src_enum,
                        conv: span.conv_id.clone(),
                        role: mur_common::Role::User,
                        content: mur_common::Content::Text {
                            value: span.text.clone(),
                        },
                        meta: serde_json::json!({ "id_suffix": span.line_hint }),
                        refs: vec![],
                    };
                    batch.push((msg, vec, 2i8));
                }
                if !batch.is_empty() {
                    let n = batch.len() as u64;
                    idx.upsert_with_layer(&batch).await?;
                    span_rows += n;
                }
            }
        }
        println!("reindexed spans: {span_rows} spans");
    }

    // Phase 3.2: rollup rebuild (layer=3 + layer=4).
    if !args.raw_only && !args.spans_only {
        use chrono::TimeZone;
        let dims: i32 = {
            let c = crate::store::config::load_config().unwrap_or_default();
            crate::store::embedding::EmbeddingConfig::from_config(&c).dimensions as i32
        };
        let mut idx = crate::conversations::index::ConversationIndex::open(dims, None).await?;
        let mut weekly_count = 0u64;
        let mut monthly_count = 0u64;

        // Weeklies
        let weekly_root = crate::conversations::paths::weekly_summary_root(None);
        if weekly_root.exists() {
            for entry in std::fs::read_dir(&weekly_root)? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let stem = stem.to_string();
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                let Ok(parsed) = crate::conversations::summarize::parse_summary(&body) else {
                    continue;
                };
                let monday = crate::conversations::summarize::windows::iso_week_monday(&stem)
                    .unwrap_or(parsed.date);
                let ts = chrono::Utc
                    .from_utc_datetime(&monday.and_hms_opt(0, 0, 0).unwrap())
                    .timestamp();
                let vec: Vec<f32> = if let Some(mode) = crate::conversations::ollama::mock_mode() {
                    crate::conversations::ollama::mock_embed_vector(
                        &parsed.narrative,
                        mode,
                        dims as usize,
                    )
                } else {
                    let c = crate::store::config::load_config().unwrap_or_default();
                    let ec = crate::store::embedding::EmbeddingConfig::from_config(&c);
                    crate::store::embedding::embed(&parsed.narrative, &ec)
                        .await
                        .unwrap_or_else(|_| vec![0.0; dims as usize])
                };
                let id = format!("wk_{stem}_L3_0");
                let conv_id = format!("week:{stem}");
                idx.upsert_rollup_row(crate::conversations::index::RollupRow {
                    id: &id,
                    ts,
                    source: "week",
                    conv_id: &conv_id,
                    layer: 3,
                    content: &parsed.narrative,
                    vector: &vec,
                })
                .await?;
                weekly_count += 1;
            }
        }

        // Monthlies
        let monthly_root = crate::conversations::paths::monthly_summary_root(None);
        if monthly_root.exists() {
            for entry in std::fs::read_dir(&monthly_root)? {
                let path = entry?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let stem = stem.to_string();
                let body = std::fs::read_to_string(&path).unwrap_or_default();
                let Ok(parsed) = crate::conversations::summarize::parse_summary(&body) else {
                    continue;
                };
                let first = crate::conversations::summarize::windows::month_first_day(&stem)
                    .unwrap_or(parsed.date);
                let ts = chrono::Utc
                    .from_utc_datetime(&first.and_hms_opt(0, 0, 0).unwrap())
                    .timestamp();
                let vec: Vec<f32> = if let Some(mode) = crate::conversations::ollama::mock_mode() {
                    crate::conversations::ollama::mock_embed_vector(
                        &parsed.narrative,
                        mode,
                        dims as usize,
                    )
                } else {
                    let c = crate::store::config::load_config().unwrap_or_default();
                    let ec = crate::store::embedding::EmbeddingConfig::from_config(&c);
                    crate::store::embedding::embed(&parsed.narrative, &ec)
                        .await
                        .unwrap_or_else(|_| vec![0.0; dims as usize])
                };
                let id = format!("mo_{stem}_L4_0");
                let conv_id = format!("month:{stem}");
                idx.upsert_rollup_row(crate::conversations::index::RollupRow {
                    id: &id,
                    ts,
                    source: "month",
                    conv_id: &conv_id,
                    layer: 4,
                    content: &parsed.narrative,
                    vector: &vec,
                })
                .await?;
                monthly_count += 1;
            }
        }
        println!("reindexed rollups: {weekly_count} weekly + {monthly_count} monthly");
    }

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
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let cfg = home.join(".mur").join("config.yaml");
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

pub struct CompactArgs {
    pub date: Option<String>,
    pub since: Option<String>,
    pub force: bool,
    pub if_stale: bool,
    pub max_days: Option<u32>,
    pub extractive_only: bool,
    pub debug_prompt: bool,
    pub skip_rollups: bool,
}

pub struct AskArgs {
    pub question: Option<String>, // was String; now Option because --show-session has no question
    pub src: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub k: usize,
    pub model: Option<String>,
    pub min_score: Option<f64>,
    pub json: bool,
    pub no_escalate: bool,
    pub debug_prompt: bool,
    pub strict_citations: bool,
    pub continue_flag: bool,
    /// Explicit new-session flag. Default is to archive + start fresh, so this
    /// is only meaningful as a clap `conflicts_with = "continue_flag"` signal.
    #[allow(dead_code)]
    pub new_flag: bool,
    pub show_session: bool,
    /// Phase 3.5.1: disable Stage 1b for this invocation (overrides
    /// `conversations.ask.summarize_hits_enabled`).
    pub no_summarize: bool,
    /// Phase 3.5.1: override the Stage 1b model for this invocation (overrides
    /// `conversations.ask.summarize_model`). `None` means "use config value, or
    /// fall back to `ask.model` per the resolver below".
    pub summarize_model: Option<String>,
}

pub async fn cmd_conversations_compact(args: CompactArgs) -> Result<()> {
    use crate::conversations::summarize;
    use chrono::NaiveDate;

    let config = crate::store::config::load_config().unwrap_or_default();
    let mut cfg = config.conversations.compact.clone();

    if args.extractive_only {
        // Crude guard rail: no abstractive model.
        let mut blanked = cfg.effective_abstractive_backend(&config.llm);
        blanked.model = String::new();
        cfg.abstractive_backend = Some(blanked);
    }
    if args.debug_prompt {
        eprintln!("(debug_prompt not yet wired to individual stages; enabling in Phase 2C)");
    }

    // Note: single-day compact via --date does NOT cascade into the rollup
    // sweep (one day can't close a week). To trigger rollups after a targeted
    // backfill, re-run `mur conversations compact` with no --date, or run
    // `mur conversations rollup --all-missing` explicitly.
    if let Some(d) = args.date {
        let date = NaiveDate::parse_from_str(&d, "%Y-%m-%d")?;
        let force = args.force || args.if_stale;
        let r = summarize::compact_day(date, force, &cfg, &config.llm, None).await?;
        println!(
            "{date}: {:?} ({} spans, {}ms)",
            r.outcome, r.extractive_spans, r.duration_ms
        );
        return Ok(());
    }

    let since = args
        .since
        .as_deref()
        .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
        .transpose()?;

    let report = summarize::compact_missing(
        &cfg,
        &config.llm,
        since,
        args.if_stale,
        args.force,
        args.max_days,
        None,
    )
    .await?;

    if report.day_reports.is_empty() {
        println!("(nothing to compact)");
        return Ok(());
    }
    for r in &report.day_reports {
        println!(
            "  {} {:?} ({} spans, {}ms)",
            r.date, r.outcome, r.extractive_spans, r.duration_ms
        );
    }
    println!(
        "done: {} ok, {} failed, {} skipped",
        report.ok, report.err, report.skipped
    );

    // Phase 3.2: cascade into rollups unless explicitly suppressed.
    if !args.skip_rollups {
        let rollup_cfg = config.conversations.rollup.clone();
        if rollup_cfg.enabled {
            println!("\nrollup sweep:");
            let sweep = crate::conversations::summarize::rollup::rollup_missing(
                &rollup_cfg,
                &config.llm,
                crate::conversations::summarize::rollup::RollupKinds::All,
                None,
                None,
                None,
            )
            .await?;
            for r in &sweep.reports {
                println!("  {} {:?} ({}ms)", r.window, r.outcome, r.duration_ms);
            }
            println!(
                "done: {} week ok / {} week err / {} week skipped; {} month ok / {} month err / {} month skipped",
                sweep.week_ok,
                sweep.week_err,
                sweep.week_skipped,
                sweep.month_ok,
                sweep.month_err,
                sweep.month_skipped,
            );
        }
    }

    Ok(())
}

/// Phase 3.5.1 — collapse CLI summarize flags + AskConfig defaults into the
/// effective `(summarize_enabled, summarize_model)` pair fed to `AskRequest`.
///
/// Precedence: CLI flag > config > hardcoded default (handled upstream by
/// `AskConfig::default`). clap rejects `--no-summarize` + `--summarize-model`
/// together at parse time (see `conflicts_with` in `main.rs`), so the
/// combination is unreachable here.
pub(crate) fn resolve_summarize(
    no_summarize: bool,
    cli_model: Option<&str>,
    cfg_enabled: bool,
    cfg_model: Option<&str>,
) -> (bool, Option<String>) {
    let enabled = !no_summarize && cfg_enabled;
    let model = cli_model
        .map(|s| s.to_string())
        .or_else(|| cfg_model.map(|s| s.to_string()));
    (enabled, model)
}

pub async fn cmd_ask(args: AskArgs) -> Result<()> {
    use crate::conversations::ask;
    use chrono::{NaiveDate, Utc};
    use futures::StreamExt;
    use std::io::Write;

    let cfg = crate::store::config::load_config().unwrap_or_default();
    let ask_cfg = cfg.conversations.ask.clone();
    let history_retain = cfg.conversations.compact.history_retain;
    let history_turns = ask_cfg.continue_history_turns;
    let effective = ask_cfg.effective_backend(&cfg.llm);

    // --show-session path: no LLM calls, early return
    if args.show_session {
        return cmd_ask_show_session(None);
    }

    let question = match args.question.clone() {
        Some(q) => q,
        None => {
            anyhow::bail!("question is required (or use --show-session to inspect current session)")
        }
    };

    // Session management
    let mut session = if args.continue_flag {
        let s = ask::session::SessionStore::load_latest(None)?;
        if s.turns.is_empty() {
            anyhow::bail!("no prior session; run without --continue to start a new one");
        }
        s
    } else {
        ask::session::SessionStore::archive_and_new(None, history_retain)?
    };

    // Rewriter call (only if continuing + we have prior turns).
    // Separate shorter timeout (ask_cfg.rewriter_timeout_secs, default 8s)
    // so a slow/unreachable Ollama doesn't burn the full generate budget
    // before the user sees a response — we fall back to the raw question
    // on timeout. The main generation path still uses its own client with
    // the full ask_cfg.timeout_secs budget (plumbed via ask_stream).
    let prior_slice = session.last_n(history_turns);
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| effective.model.clone());
    let rewriter_backend = crate::conversations::backend::factory::build_for_stage(
        &ask_cfg.effective_rewriter_backend(&cfg.llm),
        "rewriter",
    )?;
    let rewrite = ask::rewriter::rewrite(
        rewriter_backend.as_ref(),
        &model,
        ask::rewriter::RewriteInput {
            prior_turns: prior_slice,
            raw_question: &question,
        },
    )
    .await;
    let retrieval_query = rewrite.rewritten.clone();
    let rewriter_status = rewrite.status;

    // Build AskRequest
    let sources = args.src.as_deref().map(parse_sources).unwrap_or_default();
    let filters = ask::Filters {
        source: sources,
        since: args
            .since
            .as_deref()
            .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
            .transpose()?,
        until: args
            .until
            .as_deref()
            .map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d"))
            .transpose()?,
        min_score: args.min_score.unwrap_or(ask_cfg.min_score),
    };
    let (effective_summarize_enabled, effective_summarize_model) = resolve_summarize(
        args.no_summarize,
        args.summarize_model.as_deref(),
        ask_cfg.summarize_hits_enabled,
        ask_cfg.summarize_model.as_deref(),
    );
    // Build the answer-streaming backend via factory, honoring the per-stage
    // `ask.backend` override. effective_backend() bakes ask.timeout_secs into
    // the resolved BackendConfig (see I2 fix in P3 task 1) so factory's 120s
    // default doesn't override the user's per-call budget.
    //
    // P3 task 8: build TWO backends (Stage 1b + answer stream) from the same
    // BackendConfig but tag them differently for telemetry — cost-report
    // breaks down spend by stage.
    let stage1b_backend =
        crate::conversations::backend::factory::build_for_stage(&effective, "ask.compress_hit")?;
    let answer_backend =
        crate::conversations::backend::factory::build_for_stage(&effective, "ask.generate")?;
    let req = ask::AskRequest {
        question: question.clone(),
        filters,
        k_summary: args.k,
        k_raw: args.k * 2,
        escalation_threshold: ask_cfg.escalation_threshold,
        mmr_threshold: ask_cfg.mmr_threshold,
        model,
        endpoint: effective.endpoint.clone().unwrap_or_default(),
        format: if args.json {
            ask::Format::Json
        } else {
            ask::Format::Plain
        },
        max_context_tokens: ask_cfg.max_context_tokens as usize,
        response_tokens: ask_cfg.response_tokens as usize,
        timeout: std::time::Duration::from_secs(ask_cfg.timeout_secs as u64),
        no_escalate: args.no_escalate,
        debug_prompt: args.debug_prompt,
        strict_citations: args.strict_citations,
        prior_turns: prior_slice.to_vec(),
        retrieval_query,
        rewriter_status,
        compress_enabled: ask_cfg.compress_hits_enabled,
        summarize_enabled: effective_summarize_enabled,
        summarize_model: effective_summarize_model,
        answer_backend: Some(answer_backend),
        stage1b_backend: Some(stage1b_backend),
    };

    // Generate + collect response
    // streaming_error is set if the streaming branch sees an AskEvent::Error
    // *or* the JSON branch's `ask::ask` call returns Err. In either case the
    // error is surfaced after persistence so degraded turns still write to
    // the JSONL — mirroring the streaming fix in PR #15.
    let mut streaming_error: Option<String> = None;
    let resp = if args.json {
        match ask::ask(req, None).await {
            Ok(r) => {
                println!("{}", serde_json::to_string_pretty(&r)?);
                r
            }
            Err(e) => {
                streaming_error = Some(e.to_string());
                // Build a degraded AskResponse so the turn still persists with
                // the question + rewriter context. answer/hits/citations are
                // empty; degraded_to_mode_b is true so --show-session and
                // downstream analytics can count it.
                ask::AskResponse {
                    answer: String::new(),
                    citations: Vec::new(),
                    hits_used: Vec::new(),
                    degraded_to_mode_b: true,
                    tokens_in: 0,
                    tokens_out: 0,
                    duration_ms: 0,
                    rewritten_question: match rewriter_status {
                        ask::session::RewriterStatus::Skipped => None,
                        _ => Some(rewrite.rewritten.clone()),
                    },
                    rewriter_status,
                    stage_1b: None,
                }
            }
        }
    } else {
        let mut stream = ask::ask_stream(req, None).await?;
        let mut answer = String::new();
        let mut citations = Vec::new();
        let mut hits_used = Vec::new();
        let mut degraded = false;
        let mut tokens_in = 0;
        let mut tokens_out = 0;
        let mut duration = 0;
        let mut stage_1b_done: Option<crate::conversations::ask::abstractive::Stage1bStats> = None;
        // Phase 3.3 follow-up: capture (don't exit on) Error events so that
        // degraded turns under Ollama-unavailable still get persisted to the
        // session JSONL via append_turn below. The exit happens at the end
        // (after persist), preserving the original non-zero exit semantics.
        while let Some(evt) = stream.next().await {
            match evt? {
                ask::AskEvent::Token(t) => {
                    print!("{t}");
                    std::io::stdout().flush()?;
                    answer.push_str(&t);
                }
                ask::AskEvent::Citation(c) => citations.push(c),
                ask::AskEvent::HitInfo(h) => hits_used.push(h),
                ask::AskEvent::Done {
                    tokens_in: ti,
                    tokens_out: to,
                    degraded: d,
                    duration_ms,
                    stage_1b: sb,
                } => {
                    tokens_in = ti;
                    tokens_out = to;
                    degraded = d;
                    duration = duration_ms;
                    stage_1b_done = sb;
                }
                ask::AskEvent::Error(e) => {
                    streaming_error = Some(e);
                }
            }
        }
        println!();
        print!(
            "{}{}",
            crate::conversations::ask::format::render_citations_block(&citations),
            crate::conversations::ask::format::render_footer(&ask::AskResponse {
                answer: answer.clone(),
                citations: citations.clone(),
                hits_used: hits_used.clone(),
                degraded_to_mode_b: degraded,
                tokens_in,
                tokens_out,
                duration_ms: duration,
                rewritten_question: match rewriter_status {
                    ask::session::RewriterStatus::Skipped => None,
                    _ => Some(rewrite.rewritten.clone()),
                },
                rewriter_status,
                stage_1b: stage_1b_done.clone(),
            }),
        );
        ask::AskResponse {
            answer,
            citations,
            hits_used,
            degraded_to_mode_b: degraded,
            tokens_in,
            tokens_out,
            duration_ms: duration,
            rewritten_question: match rewriter_status {
                ask::session::RewriterStatus::Skipped => None,
                _ => Some(rewrite.rewritten.clone()),
            },
            rewriter_status,
            stage_1b: stage_1b_done,
        }
    };

    // Persist the turn
    let turn = ask::session::TurnRecord {
        v: 1,
        turn_id: session.next_turn_id(),
        ts: Utc::now(),
        question,
        rewritten_question: resp.rewritten_question.clone(),
        hits_used: resp.hits_used.clone(),
        answer: resp.answer.clone(),
        citations: resp.citations.clone(),
        degraded_to_mode_b: resp.degraded_to_mode_b,
        rewriter_status: resp.rewriter_status,
        tokens_in: resp.tokens_in,
        tokens_out: resp.tokens_out,
        duration_ms: resp.duration_ms,
    };
    ask::session::SessionStore::append_turn(&mut session, turn)?;

    // Phase 3.3 follow-up: report streaming-path Error events AFTER the turn
    // has been persisted. Preserves the original `--continue`-after-failure UX
    // (user sees the error message + non-zero exit) while ensuring the session
    // JSONL has a record of the degraded turn.
    if let Some(e) = streaming_error {
        eprintln!("\nerror: {e}");
        std::process::exit(1);
    }

    Ok(())
}

/// `mur ask --show-session` handler. No LLM calls.
fn cmd_ask_show_session(root_override: Option<&str>) -> Result<()> {
    let session = crate::conversations::ask::session::SessionStore::load_latest(root_override)?;
    let path = crate::conversations::paths::ask_session_path(root_override);
    if session.turns.is_empty() {
        println!("session: {}", path.display());
        println!("no active session. run 'mur ask \"question\"' to start one.");
        return Ok(());
    }
    println!("session: {}", path.display());
    println!("turns: {}", session.turns.len());
    let last = session.turns.last().unwrap();
    let now = chrono::Utc::now();
    let delta = now.signed_duration_since(last.ts);
    let delta_str = humanize_duration(delta);
    println!("last turn: {} ({delta_str})", last.ts.to_rfc3339());
    let first = &session.turns[0];
    let first_q = truncate_chars_simple(&first.question, 80);
    println!("first question: \"{first_q}\"");
    let degraded = session
        .turns
        .iter()
        .filter(|t| {
            t.degraded_to_mode_b
                || t.rewriter_status
                    == crate::conversations::ask::session::RewriterStatus::FailedFellBackToRaw
        })
        .count();
    println!("degraded turns: {degraded}");
    Ok(())
}

fn humanize_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{} minutes ago", secs / 60)
    } else if secs < 86400 {
        format!("{} hours ago", secs / 3600)
    } else {
        format!("{} days ago", secs / 86400)
    }
}

fn truncate_chars_simple(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_summarize_no_flag_uses_config_enabled_and_model() {
        let (enabled, model) = resolve_summarize(
            /* no_summarize */ false,
            /* cli_model    */ None,
            /* cfg_enabled  */ true,
            /* cfg_model    */ Some("qwen3:14b"),
        );
        assert!(enabled);
        assert_eq!(model.as_deref(), Some("qwen3:14b"));
    }

    #[test]
    fn resolve_summarize_no_summarize_flag_forces_disabled_regardless_of_config() {
        let (enabled, model) = resolve_summarize(
            /* no_summarize */ true,
            /* cli_model    */ None,
            /* cfg_enabled  */ true,
            /* cfg_model    */ Some("qwen3:14b"),
        );
        assert!(!enabled, "--no-summarize must override enabled config");
        // model still bubbles up (CLI didn't set one) — the disabled flag is what matters.
        assert_eq!(model.as_deref(), Some("qwen3:14b"));
    }

    #[test]
    fn resolve_summarize_cli_model_overrides_config_model() {
        let (enabled, model) = resolve_summarize(
            /* no_summarize */ false,
            /* cli_model    */ Some("qwen3:4b"),
            /* cfg_enabled  */ true,
            /* cfg_model    */ Some("qwen3:14b"),
        );
        assert!(enabled);
        assert_eq!(
            model.as_deref(),
            Some("qwen3:4b"),
            "CLI model wins over config"
        );
    }

    #[test]
    fn resolve_summarize_cli_model_falls_back_to_config_when_none() {
        let (enabled, model) = resolve_summarize(
            /* no_summarize */ false,
            /* cli_model    */ None,
            /* cfg_enabled  */ false,
            /* cfg_model    */ Some("qwen3:14b"),
        );
        assert!(
            !enabled,
            "config-disabled stays disabled without CLI override"
        );
        assert_eq!(model.as_deref(), Some("qwen3:14b"));
    }

    // ── Fix round 1, finding 1 pinning tests ──────────────────────────────
    //
    // Before this fix, doctor/preflight probed `ollama_backends[0].endpoint`
    // (one arbitrary endpoint) and validated every Ollama-routed model
    // against it. A model actually routed to a second endpoint was reported
    // "missing" against a host that was never queried.

    #[test]
    fn group_ollama_backends_by_endpoint_keeps_endpoints_separate() {
        let backends = vec![
            mur_common::config::BackendConfig {
                provider: "ollama".into(),
                model: "llama3:70b".into(),
                endpoint: Some("http://localhost:11434".into()),
                ..Default::default()
            },
            mur_common::config::BackendConfig {
                provider: "ollama".into(),
                model: "qwen3:4b".into(),
                endpoint: Some("http://box.local:11434".into()),
                ..Default::default()
            },
        ];
        let groups = group_ollama_backends_by_endpoint(&backends);
        assert_eq!(
            groups.len(),
            2,
            "two distinct endpoints must stay distinct: {groups:?}"
        );

        let local = groups
            .iter()
            .find(|g| g.endpoint == "http://localhost:11434")
            .expect("localhost group present");
        assert_eq!(local.models, vec!["llama3:70b".to_string()]);

        let boxed = groups
            .iter()
            .find(|g| g.endpoint == "http://box.local:11434")
            .expect("box.local group present");
        assert_eq!(boxed.models, vec!["qwen3:4b".to_string()]);

        // The bug this pins: model B must never appear under endpoint A's group.
        assert!(!local.models.contains(&"qwen3:4b".to_string()));
        assert!(!boxed.models.contains(&"llama3:70b".to_string()));
    }

    #[test]
    fn collect_backend_configs_keeps_same_model_on_different_endpoints_distinct() {
        // Two stages route the SAME model name to two DIFFERENT Ollama hosts.
        // Deduping by (provider, model) alone — the pre-fix behavior — would
        // collapse these into one entry and silently drop an endpoint from
        // the probe set.
        let shared_model = "llama3:70b";
        let cfg = mur_common::config::Config {
            conversations: mur_common::config::ConversationsConfig {
                ask: mur_common::config::AskConfig {
                    backend: Some(mur_common::config::BackendConfig {
                        provider: "ollama".into(),
                        model: shared_model.into(),
                        endpoint: Some("http://localhost:11434".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                compact: mur_common::config::CompactConfig {
                    extractive_backend: Some(mur_common::config::BackendConfig {
                        provider: "ollama".into(),
                        model: shared_model.into(),
                        endpoint: Some("http://box.local:11434".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };

        let backends = collect_backend_configs(&cfg);
        let groups = group_ollama_backends_by_endpoint(&backends);
        assert_eq!(
            groups.len(),
            2,
            "same model on two different endpoints must survive as two groups: {groups:?}"
        );

        // The bug this pins: the model routed to endpoint B (box.local) must
        // not be reported missing when only endpoint A (localhost) was
        // probed — it must show up in box.local's own group, not be
        // silently dropped by dedup.
        let box_group = groups
            .iter()
            .find(|g| g.endpoint == "http://box.local:11434")
            .expect("box.local endpoint must survive dedup, not be silently dropped");
        assert!(box_group.models.contains(&shared_model.to_string()));

        let local_group = groups
            .iter()
            .find(|g| g.endpoint == "http://localhost:11434")
            .expect("localhost endpoint must survive dedup too");
        assert!(local_group.models.contains(&shared_model.to_string()));
    }
}
