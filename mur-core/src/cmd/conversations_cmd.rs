//! CLI handlers for conversations archive commands.
//! See spec §6.

use anyhow::Result;
use chrono::NaiveDate;
use mur_common::Source;
use tracing::info;

use crate::conversations;

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

pub async fn cmd_conversations_reindex() -> Result<()> {
    let cfg = crate::store::config::load_config().unwrap_or_default();
    let embed_cfg = crate::store::embedding::EmbeddingConfig::from_config(&cfg);
    let dims = embed_cfg.dimensions as i32;
    let mut idx = conversations::index::ConversationIndex::open(dims, None).await?;
    let mut count = 0u64;
    for (date, _dir) in conversations::store::list_raw_dirs(None)? {
        let msgs = conversations::store::read_day(date, None)?;
        let mut entries = Vec::with_capacity(msgs.len());
        for m in msgs {
            let txt = match &m.content {
                mur_common::Content::Text { value } => value.clone(),
                mur_common::Content::ToolRef { desc, .. } => desc.clone(),
                mur_common::Content::ImageRef { desc, .. } => desc.clone(),
            };
            let vec = crate::store::embedding::embed(&txt, &embed_cfg).await?;
            entries.push((m, vec));
        }
        let len = entries.len() as u64;
        idx.upsert(&entries).await?;
        count += len;
    }
    println!("Reindexed {count} messages");
    Ok(())
}

pub async fn cmd_conversations_doctor() -> Result<()> {
    println!("conversations doctor");
    let dirs = conversations::store::list_raw_dirs(None).unwrap_or_default();
    println!("  ✓ raw day-dirs: {}", dirs.len());
    let audit_ok = conversations::audit::verify(None).unwrap_or(false);
    println!("  {} audit hash chain", if audit_ok { "✓" } else { "✗" });
    let cfg_days = conversations::retention::retention_days_from_config();
    println!("  ✓ retention_days = {cfg_days}");
    let enabled = conversations::is_enabled().unwrap_or(false);
    println!(
        "  {} conversations.enabled",
        if enabled { "✓" } else { "·" }
    );

    // Phase 2A additions
    let raw_dir = conversations::paths::raw_root(None);
    let summary_dir = conversations::paths::conversations_root(None).join("summary");
    let raw_days: Vec<_> = std::fs::read_dir(&raw_dir)
        .ok()
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let summary_count = std::fs::read_dir(&summary_dir)
        .ok()
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s == "md")
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0);

    let today = chrono::Utc::now().date_naive();
    let completed_days: Vec<&String> = raw_days
        .iter()
        .filter(|d| {
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .map(|pd| pd < today)
                .unwrap_or(false)
        })
        .collect();
    let missing = completed_days.len().saturating_sub(summary_count);
    if missing == 0 {
        println!("  ✓ summaries: all {summary_count} completed days covered");
    } else {
        println!(
            "  ⚠ summaries: {summary_count} of {} completed days covered — run 'mur conversations compact'",
            completed_days.len()
        );
    }

    // Ollama reachability (non-blocking 1s probe)
    let cfg = crate::store::config::load_config().unwrap_or_default();
    let endpoint = cfg.conversations.compact.ollama_endpoint.clone();
    let reachable = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        reqwest::get(format!("{}/api/tags", endpoint.trim_end_matches('/'))),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .map(|r| r.status().is_success())
    .unwrap_or(false);
    if reachable {
        println!("  ✓ Ollama reachable at {endpoint}");
    } else {
        println!("  · Ollama not reachable at {endpoint} (compact + ask will degrade)");
    }

    Ok(())
}

/// BP1 amendment: dedicated preflight check bundle (daemon, disk, staging, audit presence).
pub fn cmd_conversations_preflight() -> Result<()> {
    use crate::conversations::migrate;
    let plan = migrate::dry_run(None)?;

    let mut ok = true;
    println!("conversations preflight");
    if plan.commander_daemon_running {
        println!("  ✗ commander daemon appears to be running");
        ok = false;
    } else {
        println!("  ✓ commander daemon not running");
    }
    // Disk space check (best-effort; does not resolve mount points perfectly).
    // fs_available_bytes returns None (treated as unlimited) in Phase 1 —
    // a proper statvfs wrapper lands in a later task.
    let home = dirs::home_dir().unwrap_or_default();
    let needed = plan.free_space_needed_bytes;
    let free = fs_available_bytes(&home).unwrap_or(u64::MAX);
    if free < needed {
        println!(
            "  ✗ disk: {:.1} MB free, need {:.1} MB",
            free as f64 / 1_048_576.0,
            needed as f64 / 1_048_576.0
        );
        ok = false;
    } else {
        println!(
            "  ✓ disk: {:.1} MB free, need {:.1} MB",
            free as f64 / 1_048_576.0,
            needed as f64 / 1_048_576.0
        );
    }
    let staging = home.join(".mur/.conversations-migrating");
    if staging.exists() {
        println!(
            "  ✗ staging dir exists at {} — run migrate --resume or --discard-staging",
            staging.display()
        );
        ok = false;
    } else {
        println!("  ✓ no stale staging dir");
    }
    // Commander audit presence (not verification — different algo)
    let cmdr_audit = home.join(".mur/commander/audit.jsonl");
    if cmdr_audit.exists() {
        println!("  ✓ commander audit present (opaque bridge target)");
    } else {
        println!("  · no commander audit; migration will bridge from ZERO_HASH");
    }
    if ok {
        println!("\n→ preflight passed");
    } else {
        println!("\n✗ preflight FAILED — resolve issues above");
        std::process::exit(1);
    }
    Ok(())
}

/// Phase 1: returns None (treated as unlimited) so the disk check never blocks.
/// A proper statvfs wrapper can land in a later task.
fn fs_available_bytes(_path: &std::path::Path) -> Option<u64> {
    None
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
}

pub struct AskArgs {
    pub question: String,
    pub src: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub k: usize,
    pub model: Option<String>,
    pub min_score: Option<f64>,
    pub json: bool,
    pub no_escalate: bool,
    pub debug_prompt: bool,
}

pub async fn cmd_conversations_compact(args: CompactArgs) -> Result<()> {
    use crate::conversations::summarize;
    use chrono::NaiveDate;

    let config = crate::store::config::load_config().unwrap_or_default();
    let mut cfg = config.conversations.compact.clone();

    if args.extractive_only {
        // Crude guard rail: no abstractive model.
        cfg.abstractive_model = String::new();
    }
    if args.debug_prompt {
        eprintln!("(debug_prompt not yet wired to individual stages; enabling in Phase 2C)");
    }

    if let Some(d) = args.date {
        let date = NaiveDate::parse_from_str(&d, "%Y-%m-%d")?;
        let force = args.force || args.if_stale;
        let r = summarize::compact_day(date, force, &cfg, None).await?;
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

    let report =
        summarize::compact_missing(&cfg, since, args.if_stale, args.max_days, None).await?;

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
    Ok(())
}

pub async fn cmd_ask(args: AskArgs) -> Result<()> {
    use crate::conversations::ask;
    use chrono::NaiveDate;
    use futures::StreamExt;
    use std::io::Write;

    let cfg = crate::store::config::load_config().unwrap_or_default();
    let ask_cfg = cfg.conversations.ask.clone();
    let model = args.model.unwrap_or_else(|| ask_cfg.model.clone());

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

    let req = ask::AskRequest {
        question: args.question.clone(),
        filters,
        k_summary: args.k,
        k_raw: args.k * 2,
        escalation_threshold: ask_cfg.escalation_threshold,
        mmr_threshold: ask_cfg.mmr_threshold,
        model,
        endpoint: ask_cfg.ollama_endpoint.clone(),
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
    };

    if args.json {
        let resp = ask::ask(req, None).await?;
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        let mut stream = ask::ask_stream(req, None).await?;
        let mut citations = Vec::new();
        let mut degraded = false;
        let mut tokens_in = 0;
        let mut tokens_out = 0;
        let mut duration = 0;
        while let Some(evt) = stream.next().await {
            match evt? {
                ask::AskEvent::Token(t) => {
                    print!("{t}");
                    std::io::stdout().flush()?;
                }
                ask::AskEvent::Citation(c) => citations.push(c),
                ask::AskEvent::HitInfo(_) => {}
                ask::AskEvent::Done {
                    tokens_in: ti,
                    tokens_out: to,
                    degraded: d,
                    duration_ms,
                } => {
                    tokens_in = ti;
                    tokens_out = to;
                    degraded = d;
                    duration = duration_ms;
                }
                ask::AskEvent::Error(e) => {
                    eprintln!("\nerror: {e}");
                    std::process::exit(1);
                }
            }
        }
        println!();
        print!(
            "{}{}",
            crate::conversations::ask::format::render_citations_block(&citations),
            crate::conversations::ask::format::render_footer(&ask::AskResponse {
                answer: String::new(),
                citations: citations.clone(),
                hits_used: vec![],
                degraded_to_mode_b: degraded,
                tokens_in,
                tokens_out,
                duration_ms: duration,
            }),
        );
    }
    Ok(())
}
