use super::*;
use crate::conversations::index;
use mur_common::{Role, Source};

fn dummy_doc(date: NaiveDate) -> SummaryDoc {
    SummaryDoc {
        date,
        generated_at: Utc::now(),
        extractive_model: "qwen3:14b".into(),
        abstractive_model: "qwen3:14b".into(),
        mur_version: "2.4.0".into(),
        duration_ms: 1234,
        conv_count: 1,
        msg_count: 2,
        sources: vec!["cc".into()],
        pattern_refs: vec![],
        keywords: vec!["test".into()],
        links_prev: None,
        links_next: None,
        warnings: vec![],
        input_content_sha: "deadbeef".into(),
        extractive: vec![ExtractiveSpan {
            role: Role::User,
            conv_id: "c1".into(),
            line_hint: 1,
            text: "hello".into(),
            src: Source::ClaudeCode,
        }],
        abstractive: AbstractiveResult {
            narrative: Some("Today the developer said hello.".into()),
            word_count: 5,
        },
    }
}

#[tokio::test]
async fn writes_valid_frontmatter_body() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
    let doc = dummy_doc(date);
    let r = write_summary(&doc, vec![0.0; 16], vec![], false, Some(root))
        .await
        .unwrap();
    assert!(!r.noop);
    assert!(r.archived.is_none());
    let body = std::fs::read_to_string(&r.path).unwrap();
    assert!(body.contains("date: 2026-04-19"));
    assert!(body.contains("## Extractive spans"));
    assert!(body.contains("## Abstractive narrative"));
    assert!(body.contains("Today the developer said hello."));
}

#[tokio::test]
async fn second_identical_write_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
    let doc = dummy_doc(date);
    let mut d2 = dummy_doc(date);
    d2.generated_at = doc.generated_at; // force bit-identical
    let _ = write_summary(&doc, vec![0.0; 16], vec![], false, Some(root))
        .await
        .unwrap();
    let r2 = write_summary(&d2, vec![0.0; 16], vec![], false, Some(root))
        .await
        .unwrap();
    assert!(r2.noop);
    assert!(r2.archived.is_none());
}

#[tokio::test]
async fn overwrite_archives_prior() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
    let mut doc1 = dummy_doc(date);
    doc1.abstractive.narrative = Some("version 1".into());
    let _ = write_summary(&doc1, vec![0.0; 16], vec![], false, Some(root))
        .await
        .unwrap();
    let mut doc2 = dummy_doc(date);
    doc2.abstractive.narrative = Some("version 2".into());
    let r2 = write_summary(&doc2, vec![0.0; 16], vec![], false, Some(root))
        .await
        .unwrap();
    assert!(r2.archived.is_some());
    let hist = summary_history_dir(Some(root));
    let entries: Vec<_> = std::fs::read_dir(&hist).unwrap().collect();
    assert_eq!(entries.len(), 1);
}

#[tokio::test]
async fn audit_records_summarize_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
    let doc = dummy_doc(date);
    let _ = write_summary(&doc, vec![0.0; 16], vec![], false, Some(root))
        .await
        .unwrap();

    // Chain is intact (verify returns true) and at least one Summarize entry exists.
    assert!(
        audit::verify(Some(root)).unwrap(),
        "audit chain must verify"
    );
    let audit_path = super::super::super::paths::audit_path(Some(root));
    let body = std::fs::read_to_string(&audit_path).unwrap();
    let has_summarize = body
        .lines()
        .filter(|l| !l.trim().is_empty())
        .any(|l| l.contains("\"kind\":\"summarize\""));
    assert!(has_summarize, "audit file must record a Summarize entry");
}

#[tokio::test]
async fn history_retention_prunes_to_retain_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();

    // Seed 7 pre-existing .history/ entries directly (bypassing write_summary
    // to avoid waiting 7 seconds between ISO-second timestamps).
    let hist = summary_history_dir(Some(root));
    std::fs::create_dir_all(&hist).unwrap();
    for i in 0..7 {
        let iso = format!("2026-04-19T00-00-0{i}Z");
        std::fs::write(
            hist.join(format!("2026-04-19.{iso}.md")),
            format!("version {i}"),
        )
        .unwrap();
    }
    assert_eq!(std::fs::read_dir(&hist).unwrap().count(), 7);

    let freed = prune_history(Some(root), date, 3).unwrap();
    assert!(freed > 0, "prune should have freed bytes");
    let remaining: Vec<_> = std::fs::read_dir(&hist)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(remaining.len(), 3, "expected 3 retained, got {remaining:?}");
    // Remaining should be the 3 newest (highest ISO suffix).
    assert!(remaining.iter().any(|n| n.contains("T00-00-06Z")));
    assert!(remaining.iter().any(|n| n.contains("T00-00-05Z")));
    assert!(remaining.iter().any(|n| n.contains("T00-00-04Z")));
}

#[test]
fn history_retention_empty_dir_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
    // prune_history on non-existent .history dir must not error
    let freed = prune_history(Some(root), date, 5).unwrap();
    assert_eq!(freed, 0);
}

#[tokio::test]
async fn write_rollup_week_produces_md_and_layer_3_row() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let doc = dummy_week_rollup_doc();
    write_rollup(&doc, vec![0.1; 16], false, Some(root))
        .await
        .unwrap();

    // Disk artifact
    let p = crate::conversations::paths::weekly_summary_path_for(&doc.window_label, Some(root));
    assert!(p.exists(), "weekly md should exist at {p:?}");
    let body = std::fs::read_to_string(&p).unwrap();
    assert!(body.contains("kind: week"));
    assert!(body.contains("window: 2026-W16"));
    assert!(body.contains("## Extractive spans"));
    assert!(body.contains("## Abstractive narrative"));

    // LanceDB row
    let idx = index::ConversationIndex::open(16, Some(root))
        .await
        .unwrap();
    assert_eq!(idx.count_rows_at_layer(3).await.unwrap(), 1);
}

#[tokio::test]
async fn write_rollup_month_produces_md_and_layer_4_row() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let doc = dummy_month_rollup_doc();
    write_rollup(&doc, vec![0.1; 16], false, Some(root))
        .await
        .unwrap();
    let p = crate::conversations::paths::monthly_summary_path_for(&doc.window_label, Some(root));
    assert!(p.exists());
    let body = std::fs::read_to_string(&p).unwrap();
    assert!(body.contains("kind: month"));
    assert!(body.contains("window: 2026-04"));
    let idx = index::ConversationIndex::open(16, Some(root))
        .await
        .unwrap();
    assert_eq!(idx.count_rows_at_layer(4).await.unwrap(), 1);
}

#[tokio::test]
async fn write_rollup_idempotent_on_identical_content() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let doc = dummy_week_rollup_doc();
    let r1 = write_rollup(&doc, vec![0.1; 16], false, Some(root))
        .await
        .unwrap();
    assert!(!r1.noop);
    // Second call with identical doc (same generated_at so body is byte-identical)
    let r2 = write_rollup(&doc, vec![0.1; 16], false, Some(root))
        .await
        .unwrap();
    assert!(r2.noop, "second identical write should be noop");
}

#[tokio::test]
async fn write_rollup_force_bypasses_idempotency() {
    // `--force` must archive + rewrite even when the body is byte-identical.
    // Guards against the Windows flake where two back-to-back rollups share
    // a wall-clock-second `generated_at` and the short-circuit skips the
    // archive the user explicitly requested.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let doc = dummy_week_rollup_doc();
    let _ = write_rollup(&doc, vec![0.1; 16], false, Some(root))
        .await
        .unwrap();
    let r2 = write_rollup(&doc, vec![0.1; 16], true, Some(root))
        .await
        .unwrap();
    assert!(!r2.noop, "force=true must NOT noop on identical content");
    assert!(r2.archived.is_some(), "force=true must archive the prior");
    let hist = crate::conversations::paths::weekly_history_dir(Some(root));
    assert_eq!(std::fs::read_dir(&hist).unwrap().count(), 1);
}

#[tokio::test]
async fn write_summary_force_bypasses_idempotency() {
    // Windows CI Hardening Phase 1 — mirrors `write_rollup_force_bypasses_idempotency`.
    // Two consecutive writes with byte-identical bodies (same date, same
    // `generated_at` second) must NOT noop when force=true; must archive
    // the prior and rewrite. Guards the bug class Phase 3.5 fixed for
    // `write_rollup` from reappearing in `write_summary`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
    let doc = dummy_doc(date);
    let _ = write_summary(&doc, vec![0.0; 16], vec![], false, Some(root))
        .await
        .unwrap();
    let r2 = write_summary(&doc, vec![0.0; 16], vec![], true, Some(root))
        .await
        .unwrap();
    assert!(!r2.noop, "force=true must NOT noop on identical content");
    assert!(r2.archived.is_some(), "force=true must archive the prior");
}

#[tokio::test]
async fn write_rollup_archives_prior_on_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let doc1 = dummy_week_rollup_doc();
    let _ = write_rollup(&doc1, vec![0.1; 16], false, Some(root))
        .await
        .unwrap();
    let mut doc2 = dummy_week_rollup_doc();
    doc2.abstractive.narrative = Some("different narrative for week".into());
    let r2 = write_rollup(&doc2, vec![0.1; 16], false, Some(root))
        .await
        .unwrap();
    assert!(r2.archived.is_some());
    let hist = crate::conversations::paths::weekly_history_dir(Some(root));
    let entries: Vec<_> = std::fs::read_dir(&hist).unwrap().collect();
    assert_eq!(entries.len(), 1);
}

fn dummy_week_rollup_doc() -> RollupDoc {
    use crate::conversations::summarize::abstractive::{AbstractiveResult, RollupKind};
    RollupDoc {
        kind: RollupKind::Week,
        window_label: "2026-W16".into(),
        window_start: chrono::NaiveDate::from_ymd_opt(2026, 4, 13).unwrap(),
        source_labels: (13..=19).map(|d| format!("2026-04-{d:02}")).collect(),
        generated_at: chrono::DateTime::parse_from_rfc3339("2026-04-20T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        extractive_model: "qwen3:14b".into(),
        abstractive_model: "qwen3:14b".into(),
        mur_version: "3.0.0".into(),
        duration_ms: 2300,
        sources: vec!["cc".into()],
        pattern_refs: vec![],
        keywords: vec![],
        links_prev: Some("2026-W15".into()),
        links_next: Some("2026-W17".into()),
        warnings: vec![],
        input_content_sha: "abc123".into(),
        extractive: vec![ExtractiveSpan {
            role: Role::User,
            conv_id: "c1".into(),
            line_hint: 1,
            text: "first span".into(),
            src: Source::ClaudeCode,
        }],
        abstractive: AbstractiveResult {
            narrative: Some("This week we shipped many things.".into()),
            word_count: 7,
        },
    }
}

fn dummy_month_rollup_doc() -> RollupDoc {
    use crate::conversations::summarize::abstractive::RollupKind;
    let mut d = dummy_week_rollup_doc();
    d.kind = RollupKind::Month;
    d.window_label = "2026-04".into();
    d.window_start = chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
    d.source_labels = vec![
        "2026-W14".into(),
        "2026-W15".into(),
        "2026-W16".into(),
        "2026-W17".into(),
    ];
    d.links_prev = Some("2026-03".into());
    d.links_next = Some("2026-05".into());
    d
}

#[tokio::test]
async fn write_summary_upserts_span_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
    let mut doc = dummy_doc(date);
    // dummy_doc seeds 1 extractive span; add two more so we can assert N rows.
    doc.extractive.push(ExtractiveSpan {
        role: Role::User,
        conv_id: "c1".into(),
        line_hint: 2,
        text: "second quote".into(),
        src: Source::ClaudeCode,
    });
    doc.extractive.push(ExtractiveSpan {
        role: Role::User,
        conv_id: "c1".into(),
        line_hint: 3,
        text: "third quote".into(),
        src: Source::ClaudeCode,
    });
    let summary_vec = vec![0.1; 16];
    let span_vecs = vec![vec![0.2; 16], vec![0.3; 16], vec![0.4; 16]];
    write_summary(&doc, summary_vec, span_vecs, false, Some(root))
        .await
        .unwrap();

    let idx = index::ConversationIndex::open(16, Some(root))
        .await
        .unwrap();
    assert_eq!(
        idx.count_rows_at_layer(1).await.unwrap(),
        1,
        "one narrative row"
    );
    assert_eq!(
        idx.count_rows_at_layer(2).await.unwrap(),
        3,
        "three span rows"
    );
}

#[tokio::test]
async fn write_summary_with_empty_spans_writes_no_layer_2() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
    let mut doc = dummy_doc(date);
    doc.extractive.clear();
    write_summary(&doc, vec![0.1; 16], vec![], false, Some(root))
        .await
        .unwrap();
    let idx = index::ConversationIndex::open(16, Some(root))
        .await
        .unwrap();
    assert_eq!(idx.count_rows_at_layer(1).await.unwrap(), 1);
    assert_eq!(idx.count_rows_at_layer(2).await.unwrap(), 0);
}
