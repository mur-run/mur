//! `mur conversations doctor` — pipeline health checks: raw/summary
//! coverage, per-stage backend listing, Ollama reachability, cloud provider
//! probes, span/rollup indexing.
//!
//! Split out of the flat `conversations_cmd.rs` into its own submodule
//! (fix round 1, finding 4) to keep individual command files under the
//! repo's 800-line cap. `collect_backend_configs`, `group_ollama_backends_by_endpoint`,
//! `probe_ollama_tags`, and `OllamaProbeOutcome` stay module-private in the
//! parent `conversations_cmd` (`super`) since `doctor` and `preflight` both
//! need them (fix round 1, finding 2). The per-stage backend listing and the
//! multi-provider cloud probe live in the sibling `backends` submodule
//! (conversations backend doctor task, 2026-08-03) rather than growing this
//! file or `mod.rs` further.

use anyhow::Result;

use crate::conversations;

use super::{
    OllamaProbeOutcome, collect_backend_configs, group_ollama_backends_by_endpoint,
    probe_ollama_tags,
};

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

    // Ollama reachability (non-blocking 1s probe per distinct endpoint) — only
    // when a conversations stage actually routes through Ollama.
    let cfg = crate::store::config::load_config().unwrap_or_default();
    let backends = collect_backend_configs(&cfg);

    // Per-stage backend listing: which real call site dials which backend,
    // and whether that's a pinned per-stage override or inherited from the
    // smart slot (`config.llm`). See `backends` submodule.
    let stage_rows = super::backends::stage_backend_rows(&cfg);
    print!(
        "{}",
        super::backends::render_stage_backends_table(&stage_rows)
    );

    let ollama_groups = group_ollama_backends_by_endpoint(&backends);
    if ollama_groups.is_empty() {
        println!("  · no conversations stage routes through Ollama (skipping reachability probe)");
    } else {
        for g in &ollama_groups {
            let reachable = matches!(
                probe_ollama_tags(&g.endpoint, std::time::Duration::from_secs(1)).await,
                OllamaProbeOutcome::Reachable(_)
            );
            if reachable {
                println!("  ✓ Ollama reachable at {}", g.endpoint);
            } else {
                println!(
                    "  · Ollama not reachable at {} (compact + ask will degrade)",
                    g.endpoint
                );
            }
        }
    }

    // Cloud provider probes: openai/openrouter (including local
    // OpenAI-compatible runtimes like omlx) get a live `/models` listing
    // check; anthropic keeps its existing key-check + live reachability
    // probe; gemini gets the key-check only (no live call — see
    // `backends::probe_and_print_gemini`). Scoped to `backends`, the same
    // deduped six-stage list the Ollama probe above uses, so a cloud
    // provider no stage routes to is never reported as a failure.
    super::backends::probe_and_print_cloud_backends(&backends).await;

    // Phase 2C: .history/ coverage — how many archived summary revisions + total bytes.
    let history_dir = conversations::paths::summary_history_dir(None);
    let (hist_count, hist_bytes) = if history_dir.exists() {
        std::fs::read_dir(&history_dir)
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
                    .fold((0u64, 0u64), |(n, bytes), e| {
                        let sz = std::fs::metadata(e.path()).map(|m| m.len()).unwrap_or(0);
                        (n + 1, bytes + sz)
                    })
            })
            .unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    if hist_count == 0 {
        println!("  · .history/: empty (no summary rewrites yet)");
    } else {
        println!(
            "  ✓ .history/: {hist_count} archived revisions, {:.1} KB",
            hist_bytes as f64 / 1024.0
        );
    }

    // Phase 3.1: span (layer=2) coverage.
    let dims: i32 = {
        let c = crate::store::config::load_config().unwrap_or_default();
        crate::store::embedding::EmbeddingConfig::from_config(&c).dimensions as i32
    };
    let idx_for_count = crate::conversations::index::ConversationIndex::open(dims, None).await;
    match idx_for_count {
        Ok(idx) => {
            let n = idx.count_rows_at_layer(2).await.unwrap_or(0);
            if n > 0 {
                println!("  ✓ spans: {n} rows at layer=2");
            } else if summary_count > 0 {
                println!(
                    "  · spans: 0 indexed — run 'mur conversations reindex --spans-only' for span-level Ask retrieval"
                );
            } else {
                println!("  · spans: no summaries yet");
            }
        }
        Err(e) => {
            println!("  · spans: could not open index: {e}");
        }
    }

    // Phase 3.2: rollup coverage
    let dims: i32 = {
        let c = crate::store::config::load_config().unwrap_or_default();
        crate::store::embedding::EmbeddingConfig::from_config(&c).dimensions as i32
    };
    match crate::conversations::index::ConversationIndex::open(dims, None).await {
        Ok(idx) => {
            let weekly_count = idx.count_rows_at_layer(3).await.unwrap_or(0);
            let monthly_count = idx.count_rows_at_layer(4).await.unwrap_or(0);
            let weekly_md_root = crate::conversations::paths::weekly_summary_root(None);
            let last_weekly = if weekly_md_root.exists() {
                std::fs::read_dir(&weekly_md_root).ok().and_then(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter_map(|n| n.strip_suffix(".md").map(String::from))
                        .max()
                })
            } else {
                None
            };
            let monthly_md_root = crate::conversations::paths::monthly_summary_root(None);
            let last_monthly = if monthly_md_root.exists() {
                std::fs::read_dir(&monthly_md_root).ok().and_then(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter_map(|n| n.strip_suffix(".md").map(String::from))
                        .max()
                })
            } else {
                None
            };

            if weekly_count > 0 {
                println!(
                    "  ✓ weekly rollups: {weekly_count} rows at layer=3{}",
                    last_weekly
                        .map(|l| format!(" (last: {l})"))
                        .unwrap_or_default()
                );
            } else {
                println!(
                    "  · weekly rollups: 0 indexed — run 'mur conversations rollup --all-missing'"
                );
            }
            if monthly_count > 0 {
                println!(
                    "  ✓ monthly rollups: {monthly_count} rows at layer=4{}",
                    last_monthly
                        .map(|l| format!(" (last: {l})"))
                        .unwrap_or_default()
                );
            } else {
                println!("  · monthly rollups: no weeks yet");
            }
        }
        Err(e) => {
            println!("  · weekly rollups: could not open index: {e}");
            println!("  · monthly rollups: could not open index: {e}");
        }
    }

    Ok(())
}
