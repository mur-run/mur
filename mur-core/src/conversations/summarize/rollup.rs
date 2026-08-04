//! Phase 3.2 rollup orchestrator — weekly + monthly summary generation.
#![allow(dead_code)] // wired progressively across tests in this file.

use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Utc};
use sha2::{Digest, Sha256};
use std::time::Instant;

use super::abstractive::{RollupAbstractiveInput, RollupKind};
use super::windows::{
    iso_week_bounds, iso_week_label_for, iso_week_monday, month_first_day, month_label_for,
};
use super::writer::{RollupDoc, write_rollup};

pub struct RollupReport {
    pub window: String,
    pub outcome: RollupOutcome,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub enum RollupOutcome {
    Written { archived: bool },
    Noop,
    Skipped { reason: &'static str },
    Failed(String),
}

pub struct RollupSweepReport {
    pub week_ok: u32,
    pub week_err: u32,
    pub week_skipped: u32,
    pub month_ok: u32,
    pub month_err: u32,
    pub month_skipped: u32,
    pub reports: Vec<RollupReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollupKinds {
    WeekOnly,
    MonthOnly,
    All,
}

// ── Implementations ──────────────────────────────────────────────────────────

pub async fn rollup_week(
    iso_week: &str,
    force: bool,
    cfg: &mur_common::config::RollupConfig,
    llm: &mur_common::config::LlmConfig,
    root_override: Option<&str>,
) -> Result<RollupReport> {
    let start = Instant::now();
    let (monday, sunday) = iso_week_bounds(iso_week)?;
    let dates: Vec<NaiveDate> = (0..7).map(|i| monday + Duration::days(i)).collect();

    // Read available day summaries
    let mut prior_narratives: Vec<(String, String)> = Vec::new();
    let mut day_shas: Vec<String> = Vec::new();
    let mut missing_days = 0u32;
    for d in &dates {
        let (md_path, _) = crate::conversations::paths::summary_paths_for(*d, root_override);
        if let Ok(body) = std::fs::read_to_string(&md_path)
            && let Ok(parsed) = super::parse_summary(&body)
        {
            prior_narratives.push((d.to_string(), parsed.narrative));
            day_shas.push(
                parsed
                    .frontmatter
                    .get("input_content_sha")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
        } else {
            missing_days += 1;
        }
    }
    if prior_narratives.is_empty() {
        return Ok(RollupReport {
            window: iso_week.to_string(),
            outcome: RollupOutcome::Skipped {
                reason: "no source days",
            },
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Compute input_content_sha for idempotency
    let input_sha = {
        let mut h = Sha256::new();
        for s in &day_shas {
            h.update(s.as_bytes());
            h.update(b"\n");
        }
        hex::encode(h.finalize())
    };

    // Skip if fresh (same sha in existing file's frontmatter)
    let md_path = crate::conversations::paths::weekly_summary_path_for(iso_week, root_override);
    if !force
        && md_path.exists()
        && let Ok(existing) = std::fs::read_to_string(&md_path)
        && existing.contains(&format!("input_content_sha: {}", input_sha))
    {
        return Ok(RollupReport {
            window: iso_week.to_string(),
            outcome: RollupOutcome::Skipped {
                reason: "already fresh",
            },
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    // Collect cross-day layer=2 spans
    let ts_lo = chrono::Utc
        .from_utc_datetime(&monday.and_hms_opt(0, 0, 0).unwrap())
        .timestamp();
    let ts_hi = chrono::Utc
        .from_utc_datetime(&(sunday + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap())
        .timestamp();

    let cfg_loaded = crate::store::config::load_config().ok().unwrap_or_default();
    let mut embed_cfg = crate::store::embedding::EmbeddingConfig::from_config(&cfg_loaded);
    // Pin to the layer-2 writer's default (summarize/mod.rs): the index is
    // created at 1024 dims, so honoring a different config dim here
    // split-brains the index and fails every weekly rollup (issue #594).
    // Phase 3 unifies BOTH paths via cfg.
    embed_cfg.dimensions = 1024;
    let embed_dims = embed_cfg.dimensions as i32;
    let idx =
        crate::conversations::index::ConversationIndex::open(embed_dims, root_override).await?;
    let span_rows = idx
        .scan_rows_at_layer(2, ts_lo, ts_hi)
        .await
        .unwrap_or_default();

    // Convert to ResolvedHits so we can reuse mmr_dedupe_cosine
    use crate::conversations::ask::HitInfo;
    use crate::conversations::ask::retrieve::{ResolvedHit, mmr_dedupe_cosine, similarity_of};
    let resolved: Vec<ResolvedHit> = span_rows
        .into_iter()
        .map(|h| {
            let date = chrono::DateTime::from_timestamp(h.ts, 0)
                .map(|d| d.date_naive())
                .unwrap_or(monday);
            let line_hint =
                h.id.rsplit_once("_L2_")
                    .and_then(|(_, s)| s.parse::<u32>().ok());
            ResolvedHit {
                layer: 2,
                info: HitInfo {
                    layer: 2,
                    source: h.source.file_prefix().to_string(),
                    conv_id: h.conv_id.clone(),
                    date,
                    score: similarity_of(&h),
                },
                snippet: h.content.clone(),
                line_hint,
                span_index_in_summary: line_hint,
                vector: h.vector,
                compressed: None,
            }
        })
        .collect();

    let deduped = mmr_dedupe_cosine(resolved, cfg.week_mmr_threshold);
    let mut selected = deduped;
    selected.sort_by_key(|h| (h.info.date, h.line_hint.unwrap_or(0)));
    if selected.len() > cfg.max_extractive_spans_per_week as usize {
        selected.truncate(cfg.max_extractive_spans_per_week as usize);
    }

    // Abstractive (P3 migration: trait-based).
    let abstractive_cfg = cfg.effective_abstractive_backend(llm);
    let abstractive_backend =
        crate::conversations::backend::factory::build_for_stage(&abstractive_cfg, "rollup")?;
    let abstractive = super::abstractive::rollup_narrative(
        abstractive_backend.as_ref(),
        &abstractive_cfg.model,
        &RollupAbstractiveInput {
            kind: RollupKind::Week,
            window_label: iso_week,
            selected_spans: &selected,
            prior_narratives: &prior_narratives,
        },
        cfg.max_abstractive_words_per_week,
    )
    .await;

    let mut warnings: Vec<String> = Vec::new();
    if missing_days > 0 {
        warnings.push(format!("incomplete: missing {missing_days} of 7 days"));
    }
    if abstractive.narrative.is_none() {
        warnings.push("rollup_narrative_generation_failed".into());
    }

    // Build ExtractiveSpan values from ResolvedHits
    use crate::conversations::summarize::extractive::ExtractiveSpan;
    let extractive: Vec<ExtractiveSpan> = selected
        .iter()
        .map(|h| ExtractiveSpan {
            role: mur_common::Role::User,
            conv_id: h.info.conv_id.clone(),
            line_hint: h.line_hint.unwrap_or(0),
            text: h.snippet.clone(),
            src: mur_common::Source::from_prefix(&h.info.source)
                .unwrap_or(mur_common::Source::ClaudeCode),
        })
        .collect();

    let sources = {
        let mut s: Vec<String> = selected.iter().map(|h| h.info.source.clone()).collect();
        s.sort();
        s.dedup();
        s
    };
    let source_labels: Vec<String> = dates.iter().map(|d| d.to_string()).collect();

    // Resolve narrative embedding (reuse embed_cfg loaded above)
    let narrative_text = abstractive.narrative.as_deref().unwrap_or("");
    let narrative_embedding: Vec<f32> = if let Some(mode) =
        crate::conversations::ollama::mock_mode()
    {
        crate::conversations::ollama::mock_embed_vector(narrative_text, mode, embed_dims as usize)
    } else {
        crate::store::embedding::embed(narrative_text, &embed_cfg)
            .await
            .unwrap_or_else(|_| vec![0.0; embed_dims as usize])
    };

    let prev_week = iso_week_label_for(monday - Duration::days(7));
    let next_week = iso_week_label_for(monday + Duration::days(7));

    let extractive_cfg = cfg.effective_extractive_backend(llm);
    let doc = RollupDoc {
        kind: RollupKind::Week,
        window_label: iso_week.to_string(),
        window_start: monday,
        source_labels,
        generated_at: Utc::now(),
        extractive_model: extractive_cfg.model.clone(),
        abstractive_model: abstractive_cfg.model.clone(),
        mur_version: env!("CARGO_PKG_VERSION").to_string(),
        duration_ms: start.elapsed().as_millis() as u64,
        sources,
        pattern_refs: vec![],
        keywords: vec![],
        links_prev: Some(prev_week),
        links_next: Some(next_week),
        warnings,
        input_content_sha: input_sha,
        extractive,
        abstractive,
    };

    match write_rollup(&doc, narrative_embedding, force, root_override).await {
        Ok(w) => Ok(RollupReport {
            window: iso_week.to_string(),
            outcome: if w.noop {
                RollupOutcome::Noop
            } else {
                RollupOutcome::Written {
                    archived: w.archived.is_some(),
                }
            },
            duration_ms: start.elapsed().as_millis() as u64,
        }),
        Err(e) => Ok(RollupReport {
            window: iso_week.to_string(),
            outcome: RollupOutcome::Failed(format!("{e:#}")),
            duration_ms: start.elapsed().as_millis() as u64,
        }),
    }
}

pub async fn rollup_month(
    yyyy_mm: &str,
    force: bool,
    cfg: &mur_common::config::RollupConfig,
    llm: &mur_common::config::LlmConfig,
    root_override: Option<&str>,
) -> Result<RollupReport> {
    let start = Instant::now();
    let first_day = month_first_day(yyyy_mm)?;

    // Compute the set of ISO week labels this month touches.
    let last_day = {
        let next_month = if first_day.month() == 12 {
            NaiveDate::from_ymd_opt(first_day.year() + 1, 1, 1).unwrap()
        } else {
            NaiveDate::from_ymd_opt(first_day.year(), first_day.month() + 1, 1).unwrap()
        };
        next_month - Duration::days(1)
    };

    let mut week_labels: Vec<String> = Vec::new();
    let mut d = first_day;
    while d <= last_day {
        let lbl = iso_week_label_for(d);
        if !week_labels.contains(&lbl) {
            week_labels.push(lbl);
        }
        d += Duration::days(1);
    }

    // Read available weekly summaries
    let mut prior_narratives: Vec<(String, String)> = Vec::new();
    let mut week_shas: Vec<String> = Vec::new();
    let mut missing_weeks = 0u32;
    for w in &week_labels {
        let p = crate::conversations::paths::weekly_summary_path_for(w, root_override);
        if let Ok(body) = std::fs::read_to_string(&p)
            && let Ok(parsed) = super::parse_summary(&body)
        {
            prior_narratives.push((w.clone(), parsed.narrative));
            week_shas.push(
                parsed
                    .frontmatter
                    .get("input_content_sha")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
        } else {
            missing_weeks += 1;
        }
    }
    if prior_narratives.is_empty() {
        return Ok(RollupReport {
            window: yyyy_mm.to_string(),
            outcome: RollupOutcome::Skipped {
                reason: "no source weeks",
            },
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    let input_sha = {
        let mut h = Sha256::new();
        for s in &week_shas {
            h.update(s.as_bytes());
            h.update(b"\n");
        }
        hex::encode(h.finalize())
    };

    let md_path = crate::conversations::paths::monthly_summary_path_for(yyyy_mm, root_override);
    if !force
        && md_path.exists()
        && let Ok(existing) = std::fs::read_to_string(&md_path)
        && existing.contains(&format!("input_content_sha: {}", input_sha))
    {
        return Ok(RollupReport {
            window: yyyy_mm.to_string(),
            outcome: RollupOutcome::Skipped {
                reason: "already fresh",
            },
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    let ts_lo = chrono::Utc
        .from_utc_datetime(&first_day.and_hms_opt(0, 0, 0).unwrap())
        .timestamp();
    let ts_hi = chrono::Utc
        .from_utc_datetime(&(last_day + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap())
        .timestamp();

    let cfg_loaded = crate::store::config::load_config().ok().unwrap_or_default();
    let mut embed_cfg = crate::store::embedding::EmbeddingConfig::from_config(&cfg_loaded);
    // Pin to the layer-2 writer's default (summarize/mod.rs): the index is
    // created at 1024 dims, so honoring a different config dim here
    // split-brains the index and fails every weekly rollup (issue #594).
    // Phase 3 unifies BOTH paths via cfg.
    embed_cfg.dimensions = 1024;
    let embed_dims = embed_cfg.dimensions as i32;
    let idx =
        crate::conversations::index::ConversationIndex::open(embed_dims, root_override).await?;
    let span_rows = idx
        .scan_rows_at_layer(2, ts_lo, ts_hi)
        .await
        .unwrap_or_default();

    use crate::conversations::ask::HitInfo;
    use crate::conversations::ask::retrieve::{ResolvedHit, mmr_dedupe_cosine, similarity_of};
    let resolved: Vec<ResolvedHit> = span_rows
        .into_iter()
        .map(|h| {
            let date = chrono::DateTime::from_timestamp(h.ts, 0)
                .map(|d| d.date_naive())
                .unwrap_or(first_day);
            let line_hint =
                h.id.rsplit_once("_L2_")
                    .and_then(|(_, s)| s.parse::<u32>().ok());
            ResolvedHit {
                layer: 2,
                info: HitInfo {
                    layer: 2,
                    source: h.source.file_prefix().to_string(),
                    conv_id: h.conv_id.clone(),
                    date,
                    score: similarity_of(&h),
                },
                snippet: h.content.clone(),
                line_hint,
                span_index_in_summary: line_hint,
                vector: h.vector,
                compressed: None,
            }
        })
        .collect();

    let deduped = mmr_dedupe_cosine(resolved, cfg.month_mmr_threshold);
    let mut selected = deduped;
    selected.sort_by_key(|h| (h.info.date, h.line_hint.unwrap_or(0)));
    if selected.len() > cfg.max_extractive_spans_per_month as usize {
        selected.truncate(cfg.max_extractive_spans_per_month as usize);
    }

    // Abstractive (P3 migration: trait-based).
    let abstractive_cfg = cfg.effective_abstractive_backend(llm);
    let abstractive_backend =
        crate::conversations::backend::factory::build_for_stage(&abstractive_cfg, "rollup")?;
    let abstractive = super::abstractive::rollup_narrative(
        abstractive_backend.as_ref(),
        &abstractive_cfg.model,
        &RollupAbstractiveInput {
            kind: RollupKind::Month,
            window_label: yyyy_mm,
            selected_spans: &selected,
            prior_narratives: &prior_narratives,
        },
        cfg.max_abstractive_words_per_month,
    )
    .await;

    let mut warnings: Vec<String> = Vec::new();
    if missing_weeks > 0 {
        warnings.push(format!("incomplete: missing {missing_weeks} weeks"));
    }
    if abstractive.narrative.is_none() {
        warnings.push("rollup_narrative_generation_failed".into());
    }

    use crate::conversations::summarize::extractive::ExtractiveSpan;
    let extractive: Vec<ExtractiveSpan> = selected
        .iter()
        .map(|h| ExtractiveSpan {
            role: mur_common::Role::User,
            conv_id: h.info.conv_id.clone(),
            line_hint: h.line_hint.unwrap_or(0),
            text: h.snippet.clone(),
            src: mur_common::Source::from_prefix(&h.info.source)
                .unwrap_or(mur_common::Source::ClaudeCode),
        })
        .collect();

    let sources = {
        let mut s: Vec<String> = selected.iter().map(|h| h.info.source.clone()).collect();
        s.sort();
        s.dedup();
        s
    };

    // Resolve narrative embedding (reuse embed_cfg loaded above)
    let narrative_text = abstractive.narrative.as_deref().unwrap_or("");
    let narrative_embedding: Vec<f32> = if let Some(mode) =
        crate::conversations::ollama::mock_mode()
    {
        crate::conversations::ollama::mock_embed_vector(narrative_text, mode, embed_dims as usize)
    } else {
        crate::store::embedding::embed(narrative_text, &embed_cfg)
            .await
            .unwrap_or_else(|_| vec![0.0; embed_dims as usize])
    };

    let prev_month = {
        let p = if first_day.month() == 1 {
            NaiveDate::from_ymd_opt(first_day.year() - 1, 12, 1).unwrap()
        } else {
            NaiveDate::from_ymd_opt(first_day.year(), first_day.month() - 1, 1).unwrap()
        };
        month_label_for(p)
    };
    let next_month = {
        let n = if first_day.month() == 12 {
            NaiveDate::from_ymd_opt(first_day.year() + 1, 1, 1).unwrap()
        } else {
            NaiveDate::from_ymd_opt(first_day.year(), first_day.month() + 1, 1).unwrap()
        };
        month_label_for(n)
    };

    let extractive_cfg = cfg.effective_extractive_backend(llm);
    let doc = RollupDoc {
        kind: RollupKind::Month,
        window_label: yyyy_mm.to_string(),
        window_start: first_day,
        source_labels: week_labels,
        generated_at: Utc::now(),
        extractive_model: extractive_cfg.model.clone(),
        abstractive_model: abstractive_cfg.model.clone(),
        mur_version: env!("CARGO_PKG_VERSION").to_string(),
        duration_ms: start.elapsed().as_millis() as u64,
        sources,
        pattern_refs: vec![],
        keywords: vec![],
        links_prev: Some(prev_month),
        links_next: Some(next_month),
        warnings,
        input_content_sha: input_sha,
        extractive,
        abstractive,
    };

    match write_rollup(&doc, narrative_embedding, force, root_override).await {
        Ok(w) => Ok(RollupReport {
            window: yyyy_mm.to_string(),
            outcome: if w.noop {
                RollupOutcome::Noop
            } else {
                RollupOutcome::Written {
                    archived: w.archived.is_some(),
                }
            },
            duration_ms: start.elapsed().as_millis() as u64,
        }),
        Err(e) => Ok(RollupReport {
            window: yyyy_mm.to_string(),
            outcome: RollupOutcome::Failed(format!("{e:#}")),
            duration_ms: start.elapsed().as_millis() as u64,
        }),
    }
}

pub async fn rollup_missing(
    cfg: &mur_common::config::RollupConfig,
    llm: &mur_common::config::LlmConfig,
    kinds: RollupKinds,
    max_weeks_override: Option<u32>,
    max_months_override: Option<u32>,
    root_override: Option<&str>,
) -> Result<RollupSweepReport> {
    let mut report = RollupSweepReport {
        week_ok: 0,
        week_err: 0,
        week_skipped: 0,
        month_ok: 0,
        month_err: 0,
        month_skipped: 0,
        reports: Vec::new(),
    };

    let today = Utc::now().date_naive();

    // --- Weeks ---
    if matches!(kinds, RollupKinds::WeekOnly | RollupKinds::All) {
        let cap = max_weeks_override.unwrap_or(cfg.max_weeks_per_run) as usize;
        let summary_root = crate::conversations::paths::summary_root(root_override);
        let mut week_candidates: Vec<String> = Vec::new();
        if summary_root.exists() {
            for entry in std::fs::read_dir(&summary_root)?.filter_map(|e| e.ok()) {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Ok(d) = NaiveDate::parse_from_str(stem, "%Y-%m-%d") else {
                    continue;
                };
                // Closed weeks only — Sunday < today
                let w_lbl = iso_week_label_for(d);
                let w_mon = iso_week_monday(&w_lbl).unwrap_or(d);
                let w_sun = w_mon + Duration::days(6);
                if w_sun < today && !week_candidates.contains(&w_lbl) {
                    week_candidates.push(w_lbl);
                }
            }
        }
        week_candidates.sort();

        let mut taken = 0;
        for w in week_candidates {
            if taken >= cap {
                break;
            }
            let r = rollup_week(&w, false, cfg, llm, root_override).await?;
            match &r.outcome {
                RollupOutcome::Written { .. } | RollupOutcome::Noop => report.week_ok += 1,
                RollupOutcome::Failed(_) => report.week_err += 1,
                RollupOutcome::Skipped { .. } => report.week_skipped += 1,
            }
            // Only count Written against the throttle (Skipped/Noop are free)
            if matches!(r.outcome, RollupOutcome::Written { .. }) {
                taken += 1;
            }
            report.reports.push(r);
        }
    }

    // --- Months ---
    if matches!(kinds, RollupKinds::MonthOnly | RollupKinds::All) {
        let cap = max_months_override.unwrap_or(cfg.max_months_per_run) as usize;
        let weekly_root = crate::conversations::paths::weekly_summary_root(root_override);
        let mut month_candidates: Vec<String> = Vec::new();
        if weekly_root.exists() {
            for entry in std::fs::read_dir(&weekly_root)?.filter_map(|e| e.ok()) {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) != Some("md") {
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Ok(mon) = iso_week_monday(stem) else {
                    continue;
                };
                let m_lbl = month_label_for(mon);
                if let Ok(first) = month_first_day(&m_lbl) {
                    let last = if first.month() == 12 {
                        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1).unwrap() - Duration::days(1)
                    } else {
                        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1).unwrap()
                            - Duration::days(1)
                    };
                    if last < today && !month_candidates.contains(&m_lbl) {
                        month_candidates.push(m_lbl);
                    }
                }
            }
        }
        month_candidates.sort();

        let mut taken = 0;
        for m in month_candidates {
            if taken >= cap {
                break;
            }
            let r = rollup_month(&m, false, cfg, llm, root_override).await?;
            match &r.outcome {
                RollupOutcome::Written { .. } | RollupOutcome::Noop => report.month_ok += 1,
                RollupOutcome::Failed(_) => report.month_err += 1,
                RollupOutcome::Skipped { .. } => report.month_skipped += 1,
            }
            if matches!(r.outcome, RollupOutcome::Written { .. }) {
                taken += 1;
            }
            report.reports.push(r);
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Message, Role, Source};

    /// Seed a day summary for `date` with one extractive span. Also seeds the
    /// corresponding layer=2 span row in LanceDB so rollup_week can pull it
    /// via scan_rows_at_layer.
    async fn seed_day_for_rollup(root: &str, date: NaiveDate, span_text: &str) {
        // Write the summary .md
        let (md, _) = crate::conversations::paths::summary_paths_for(date, Some(root));
        if let Some(p) = md.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(
            &md,
            format!(
                "---\n\
                 schema: 1\n\
                 date: {date}\n\
                 generated_at: {date}T03:00:00Z\n\
                 generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
                 duration_ms: 50\n\
                 conv_count: 1\n\
                 msg_count: 1\n\
                 sources: [cc]\n\
                 pattern_refs: []\n\
                 keywords: []\n\
                 links:\n  prev: null\n  next: null\n\
                 warnings: []\n\
                 input_content_sha: {date}-sha\n\
                 ---\n\n\
                 ## Extractive spans\n\n\
                 [1] _{{cc/c1 @L1}}_:\n> {span_text}\n\n\
                 ## Abstractive narrative\n\n\
                 Mock narrative for {date}.\n",
            ),
        )
        .unwrap();

        // Seed a layer=2 row at ts = date midnight UTC.
        // Use 1024 dims to match rollup_week's EmbeddingConfig::default().dimensions.
        let embed_dims = 1024usize;
        let mut idx =
            crate::conversations::index::ConversationIndex::open(embed_dims as i32, Some(root))
                .await
                .unwrap();
        let m = Message {
            v: 1,
            ts: chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap()),
            src: Source::ClaudeCode,
            conv: "c1".into(),
            role: Role::User,
            content: Content::Text {
                value: span_text.into(),
            },
            meta: serde_json::json!({ "id_suffix": 1 }),
            refs: vec![],
        };
        // Hash-mode vector so cross-day MMR has distinct inputs
        let v = crate::conversations::ollama::mock_embed_vector(
            span_text,
            crate::conversations::ollama::MockMode::Hash,
            embed_dims,
        );
        idx.upsert_with_layer(&[(m, v, 2)]).await.unwrap();
    }

    fn cfg() -> mur_common::config::RollupConfig {
        mur_common::config::RollupConfig::default()
    }

    fn llm() -> mur_common::config::LlmConfig {
        mur_common::config::LlmConfig::default()
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn rollup_week_produces_layer_3_row_and_md() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        // 2026-W16 = Apr 13..19
        for d in 13..=19 {
            let date = NaiveDate::from_ymd_opt(2026, 4, d).unwrap();
            seed_day_for_rollup(root, date, &format!("day {d} span text")).await;
        }
        let report = rollup_week("2026-W16", false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
        assert!(
            matches!(report.outcome, RollupOutcome::Written { .. }),
            "expected Written, got {:?}",
            report.outcome
        );
        let idx = crate::conversations::index::ConversationIndex::open(1024, Some(root))
            .await
            .unwrap();
        assert_eq!(idx.count_rows_at_layer(3).await.unwrap(), 1);
        let p = crate::conversations::paths::weekly_summary_path_for("2026-W16", Some(root));
        assert!(p.exists());
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn rollup_week_skips_when_no_source_days() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let report = rollup_week("2026-W16", false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
        assert!(matches!(report.outcome, RollupOutcome::Skipped { .. }));
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn rollup_week_noop_on_second_identical_call() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        for d in 13..=19 {
            let date = NaiveDate::from_ymd_opt(2026, 4, d).unwrap();
            seed_day_for_rollup(root, date, &format!("day {d} span")).await;
        }
        let _ = rollup_week("2026-W16", false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
        // Second call with no changes — should skip due to matching input_content_sha
        let r2 = rollup_week("2026-W16", false, &cfg(), &llm(), Some(root))
            .await
            .unwrap();
        // Hot path: the sha-based idempotency check in rollup_week fires
        // before reaching write_rollup, so only Skipped{already fresh} is
        // reachable here. Noop (from byte-equal write_rollup comparison) is
        // dead code in this flow — keep the assertion tight so a regression
        // that bypasses the sha check would be caught.
        assert!(
            matches!(
                r2.outcome,
                RollupOutcome::Skipped {
                    reason: "already fresh"
                }
            ),
            "expected Skipped {{ already fresh }}, got {:?}",
            r2.outcome
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn rollup_missing_respects_week_throttle() {
        let _env_guard = crate::conversations::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("MUR_OLLAMA_MOCK", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        // Seed 21 days covering 3 full ISO weeks that are clearly in the past
        // (all Sundays before today = 2026-04-21).
        // 2026-W01 = Jan 5..11, 2026-W02 = Jan 12..18, 2026-W03 = Jan 19..25.
        for d in 5..=25 {
            let date = NaiveDate::from_ymd_opt(2026, 1, d).unwrap();
            seed_day_for_rollup(root, date, &format!("jan day {d}")).await;
        }
        let mut c = cfg();
        c.max_weeks_per_run = 2;
        let sweep = rollup_missing(&c, &llm(), RollupKinds::WeekOnly, None, None, Some(root))
            .await
            .unwrap();
        assert_eq!(sweep.week_ok, 2, "throttle=2 should write 2 weeks");
        // Second invocation should pick up the remaining week (W03)
        let sweep2 = rollup_missing(&c, &llm(), RollupKinds::WeekOnly, None, None, Some(root))
            .await
            .unwrap();
        assert!(
            sweep2.week_ok >= 1,
            "second sweep should write at least 1 remaining week"
        );
        unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    }
}
