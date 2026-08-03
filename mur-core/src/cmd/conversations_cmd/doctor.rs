//! `mur conversations doctor` — pipeline health checks: raw/summary
//! coverage, Ollama reachability, cloud provider probes, span/rollup
//! indexing.
//!
//! Split out of the flat `conversations_cmd.rs` into its own submodule
//! (fix round 1, finding 4) to keep individual command files under the
//! repo's 800-line cap. `collect_backend_configs`, `group_ollama_backends_by_endpoint`,
//! `probe_ollama_tags`, and `OllamaProbeOutcome` stay module-private in the
//! parent `conversations_cmd` (`super`) since `doctor` and `preflight` both
//! need them (fix round 1, finding 2).

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

    // P1: Cloud provider probes (anthropic only for P1)
    let cloud_backends: Vec<_> = backends
        .iter()
        .filter(|b| b.provider == "anthropic")
        .collect();
    if cloud_backends.is_empty() {
        println!("  · no cloud providers in active config (skipping cloud probes)");
    } else {
        for b in cloud_backends {
            // Env-var check first
            let key_env = match b.api_key_env.as_deref() {
                Some(e) => e,
                None => {
                    println!(
                        "  ✗ anthropic backend for {} has no api_key_env in config",
                        b.model
                    );
                    continue;
                }
            };
            let key = match std::env::var(key_env) {
                Ok(v) if !v.is_empty() => {
                    println!("  ✓ anthropic api_key_env {key_env} is set");
                    v
                }
                _ => {
                    println!("  ✗ anthropic api_key_env {key_env} is unset or empty");
                    continue;
                }
            };
            // Reachability + model-existence probe (2s timeout, non-fatal)
            let endpoint = b.endpoint.as_deref().unwrap_or("https://api.anthropic.com");
            let url = format!("{}/v1/models/{}", endpoint.trim_end_matches('/'), b.model);
            let probe = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                reqwest::Client::new()
                    .get(&url)
                    .header("x-api-key", &key)
                    .header("anthropic-version", "2023-06-01")
                    .send(),
            )
            .await;
            match probe {
                Ok(Ok(r)) if r.status().is_success() => {
                    println!("  ✓ anthropic model {} reachable at {endpoint}", b.model);
                }
                Ok(Ok(r)) => {
                    println!(
                        "  ✗ anthropic model {} returned {} at {endpoint}",
                        b.model,
                        r.status()
                    );
                }
                Ok(Err(e)) => {
                    println!("  ✗ anthropic probe for {} failed: {e}", b.model);
                }
                Err(_) => {
                    println!(
                        "  · anthropic probe for {} timed out at {endpoint} (2s)",
                        b.model
                    );
                }
            }
        }
    }

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
