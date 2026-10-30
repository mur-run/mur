//! `mur conversations reindex` handler.

use super::*;

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
        if idx.drop_if_unreadable().await? {
            eprintln!("note: dropped unreadable conversations index; rebuilding all layers");
        }
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
