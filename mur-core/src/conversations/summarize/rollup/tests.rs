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
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_OLLAMA_MOCK", "1");
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
    envg.unset_var("MUR_OLLAMA_MOCK");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn rollup_week_skips_when_no_source_days() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_OLLAMA_MOCK", "1");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let report = rollup_week("2026-W16", false, &cfg(), &llm(), Some(root))
        .await
        .unwrap();
    assert!(matches!(report.outcome, RollupOutcome::Skipped { .. }));
    envg.unset_var("MUR_OLLAMA_MOCK");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn rollup_week_noop_on_second_identical_call() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_OLLAMA_MOCK", "1");
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
    envg.unset_var("MUR_OLLAMA_MOCK");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn rollup_missing_respects_week_throttle() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var("MUR_OLLAMA_MOCK", "1");
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
    envg.unset_var("MUR_OLLAMA_MOCK");
}
