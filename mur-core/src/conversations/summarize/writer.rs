//! Atomic summary writer + .history/ archive + audit + LanceDB upsert.
//! Spec §4.7. Each write:
//!   1. Render Markdown (frontmatter + extractive + abstractive + macro map)
//!   2. If file exists with different content: move to .history/<date>.<iso>.md
//!   3. If file exists with identical content: no-op
//!   4. Atomic write via tmp+rename
//!   5. audit::Audit::append(Summarize{...})
//!   6. LanceDB upsert at layer=1

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::super::audit::{self, AuditAction};
use super::super::index::ConversationIndex;
use super::super::paths::{summary_history_dir, summary_paths_for};
use super::abstractive::AbstractiveResult;
use super::extractive::ExtractiveSpan;
use super::macro_refs::MacroRef;

pub struct SummaryDoc {
    pub date: NaiveDate,
    pub generated_at: DateTime<Utc>,
    pub extractive_model: String,
    pub abstractive_model: String,
    pub mur_version: String,
    pub duration_ms: u64,
    pub conv_count: u32,
    pub msg_count: u32,
    pub sources: Vec<String>, // file_prefix strings, sorted+dedup
    pub pattern_refs: Vec<MacroRef>,
    pub keywords: Vec<String>,
    pub links_prev: Option<NaiveDate>,
    pub links_next: Option<NaiveDate>,
    pub warnings: Vec<String>,
    pub input_content_sha: String,
    pub extractive: Vec<ExtractiveSpan>,
    pub abstractive: AbstractiveResult,
}

pub struct WriteResult {
    pub path: PathBuf,
    pub archived: Option<PathBuf>, // Some(path) if prior version was moved
    pub noop: bool,                // true when content was byte-identical
}

pub async fn write_summary(
    doc: &SummaryDoc,
    summary_embedding: Vec<f32>,
    span_embeddings: Vec<Vec<f32>>,
    force: bool,
    root_override: Option<&str>,
) -> Result<WriteResult> {
    let (md_path, _yaml_path) = summary_paths_for(doc.date, root_override);
    if let Some(parent) = md_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let new_body = render(doc);

    let prior_exists = md_path.exists();
    let archived;
    let noop;

    if prior_exists {
        let existing = std::fs::read_to_string(&md_path)?;
        // Byte-equality short-circuit is a best-effort optimization for the
        // "rerun with same inputs" path. `--force` bypasses it — users who
        // pass --force explicitly want a fresh archive + rewrite regardless
        // of whether the body happens to be byte-identical (e.g. two runs
        // in the same wall-clock second producing the same generated_at).
        // Mirrors the fix in `write_rollup` (Phase 3.5 post-review).
        if !force && existing == new_body {
            return Ok(WriteResult {
                path: md_path,
                archived: None,
                noop: true,
            });
        }
        archived = Some(archive_prior(&md_path, root_override)?);
        // Phase 2C: keep at most `history_retain` versions per day. Config load
        // failure is non-fatal; fall back to a sane default (5).
        let retain = crate::store::config::load_config()
            .map(|c| c.conversations.compact.history_retain)
            .unwrap_or(5);
        let _ = prune_history(root_override, doc.date, retain);
        noop = false;
    } else {
        archived = None;
        noop = false;
    }

    let tmp = md_path.with_file_name(format!(".tmp.{}.md", doc.date.format("%Y-%m-%d")));
    let mut f = std::fs::File::create(&tmp).with_context(|| format!("open tmp {tmp:?}"))?;
    f.write_all(new_body.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &md_path)?;

    // Content hash for audit
    let mut h = Sha256::new();
    h.update(new_body.as_bytes());
    let content_sha = hex::encode(h.finalize());

    let audit_log = audit::Audit::open(root_override)?;
    audit_log.append(
        AuditAction::Summarize {
            date: doc.date.format("%Y-%m-%d").to_string(),
            model: doc.abstractive_model.clone(),
            duration_ms: doc.duration_ms,
        },
        content_sha,
    )?;

    // Index upsert at layer=1
    let mut idx = ConversationIndex::open(summary_embedding.len() as i32, root_override).await?;
    let summary_msg = summary_row_as_message(doc);
    idx.upsert_with_layer(&[(summary_msg, summary_embedding, 1)])
        .await?;

    // Phase 3.1: one row per extractive span at layer=2.
    if !doc.extractive.is_empty() && doc.extractive.len() == span_embeddings.len() {
        use chrono::TimeZone;
        let span_ts = chrono::Utc.from_utc_datetime(&doc.date.and_hms_opt(0, 0, 0).unwrap());
        let mut batch: Vec<(mur_common::Message, Vec<f32>, i8)> =
            Vec::with_capacity(doc.extractive.len());
        for (span, vec) in doc.extractive.iter().zip(span_embeddings) {
            let msg = mur_common::Message {
                v: 1,
                ts: span_ts,
                src: span.src,
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
        idx.upsert_with_layer(&batch).await?;
    }

    Ok(WriteResult {
        path: md_path,
        archived,
        noop,
    })
}

/// Build a synthetic Message representing the summary for LanceDB storage.
/// The index row uses the abstractive narrative as content so retrieval's
/// keyword/MMR reranking has real text to work with.
fn summary_row_as_message(doc: &SummaryDoc) -> mur_common::Message {
    use chrono::TimeZone;
    let ts = chrono::Utc.from_utc_datetime(&doc.date.and_hms_opt(0, 0, 0).unwrap());
    let content_text = doc
        .abstractive
        .narrative
        .clone()
        .unwrap_or_else(|| "(no narrative)".to_string());
    mur_common::Message {
        v: 1,
        ts,
        src: mur_common::Source::ClaudeCode, // placeholder; summaries aggregate across sources
        conv: format!("summary:{}", doc.date.format("%Y-%m-%d")),
        role: mur_common::Role::System,
        content: mur_common::Content::Text {
            value: content_text,
        },
        meta: serde_json::json!({
            "layer": 1,
            "sources": doc.sources,
            "conv_count": doc.conv_count,
        }),
        refs: doc.pattern_refs.iter().map(|r| r.name.clone()).collect(),
    }
}

fn archive_prior(md_path: &Path, root_override: Option<&str>) -> Result<PathBuf> {
    let hist = summary_history_dir(root_override);
    std::fs::create_dir_all(&hist)?;
    let stem = md_path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("stem")?;
    let now = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let dest = hist.join(format!("{stem}.{now}.md"));
    std::fs::rename(md_path, &dest).with_context(|| format!("archive {md_path:?} → {dest:?}"))?;
    Ok(dest)
}

/// Drop oldest `.history/<date>.*` entries beyond `retain`. Returns bytes freed.
/// Filename format (from archive_prior): `<date>.<ISO>.md` — ISO-8601 seconds
/// sort chronologically when ordered lexically, so .sort() + drop-first N gives
/// the oldest.
fn prune_history(root_override: Option<&str>, date: NaiveDate, retain: u32) -> Result<u64> {
    let hist = summary_history_dir(root_override);
    if !hist.exists() {
        return Ok(0);
    }
    let stem = date.format("%Y-%m-%d").to_string();
    let mut matches: Vec<std::path::PathBuf> = std::fs::read_dir(&hist)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&stem))
                .unwrap_or(false)
        })
        .collect();
    if matches.len() <= retain as usize {
        return Ok(0);
    }
    matches.sort();
    let drop_count = matches.len() - retain as usize;
    let mut freed = 0u64;
    for p in matches.into_iter().take(drop_count) {
        if let Ok(meta) = std::fs::metadata(&p) {
            freed += meta.len();
        }
        std::fs::remove_file(&p)?;
    }
    if freed > 0 {
        let audit_log = audit::Audit::open(root_override)?;
        let _ = audit_log.append(
            AuditAction::Delete {
                target: hist.to_string_lossy().into_owned(),
                reason: "history.rotate".into(),
                bytes_freed: freed,
            },
            String::new(),
        );
    }
    Ok(freed)
}

fn render(doc: &SummaryDoc) -> String {
    let mut out = String::new();

    // Frontmatter
    out.push_str("---\n");
    out.push_str("schema: 1\n");
    out.push_str(&format!("date: {}\n", doc.date));
    out.push_str(&format!(
        "generated_at: {}\n",
        doc.generated_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    out.push_str("generated_by:\n");
    out.push_str(&format!("  extractive_model: {}\n", doc.extractive_model));
    out.push_str(&format!("  abstractive_model: {}\n", doc.abstractive_model));
    out.push_str(&format!("  mur_version: {}\n", doc.mur_version));
    out.push_str(&format!("duration_ms: {}\n", doc.duration_ms));
    out.push_str(&format!("conv_count: {}\n", doc.conv_count));
    out.push_str(&format!("msg_count: {}\n", doc.msg_count));
    out.push_str(&format!("sources: [{}]\n", doc.sources.join(", ")));
    if doc.pattern_refs.is_empty() {
        out.push_str("pattern_refs: []\n");
    } else {
        out.push_str("pattern_refs:\n");
        for r in &doc.pattern_refs {
            out.push_str(&format!(
                "  - name: {}\n    version: {}\n    sha: {}\n",
                r.name, r.pattern_version, r.pattern_sha
            ));
        }
    }
    out.push_str(&format!("keywords: [{}]\n", doc.keywords.join(", ")));
    out.push_str("links:\n");
    out.push_str(&format!(
        "  prev: {}\n",
        doc.links_prev
            .map(|d| format!("./{}.md", d))
            .unwrap_or_else(|| "null".into())
    ));
    out.push_str(&format!(
        "  next: {}\n",
        doc.links_next
            .map(|d| format!("./{}.md", d))
            .unwrap_or_else(|| "null".into())
    ));
    if doc.warnings.is_empty() {
        out.push_str("warnings: []\n");
    } else {
        out.push_str("warnings:\n");
        for w in &doc.warnings {
            out.push_str(&format!("  - {}\n", w));
        }
    }
    out.push_str(&format!("input_content_sha: {}\n", doc.input_content_sha));
    out.push_str("---\n\n");

    // Body
    out.push_str("## Extractive spans\n\n");
    for (i, s) in doc.extractive.iter().enumerate() {
        out.push_str(&format!(
            "[{}] _{{{}/{} @L{}}}_:\n> {}\n\n",
            i + 1,
            s.src.file_prefix(),
            s.conv_id,
            s.line_hint,
            s.text.replace('\n', "\n> ")
        ));
    }

    out.push_str("## Abstractive narrative\n\n");
    let narrative = doc
        .abstractive
        .narrative
        .as_deref()
        .unwrap_or("(narrative generation failed; see warnings)");
    out.push_str(narrative);
    out.push_str("\n\n");

    if !doc.pattern_refs.is_empty() {
        out.push_str("## Macro expansion map\n\n");
        for r in &doc.pattern_refs {
            out.push_str(&format!(
                "- {} → patterns/{}.yaml (v{}, sha {}…)\n",
                r.marker,
                r.name,
                r.pattern_version,
                r.pattern_sha.chars().take(8).collect::<String>()
            ));
        }
    }
    out
}

pub struct RollupDoc {
    pub kind: crate::conversations::summarize::abstractive::RollupKind,
    pub window_label: String,
    pub window_start: NaiveDate,
    pub source_labels: Vec<String>,
    pub generated_at: DateTime<Utc>,
    pub extractive_model: String,
    pub abstractive_model: String,
    pub mur_version: String,
    pub duration_ms: u64,
    pub sources: Vec<String>,
    pub pattern_refs: Vec<MacroRef>,
    pub keywords: Vec<String>,
    pub links_prev: Option<String>,
    pub links_next: Option<String>,
    pub warnings: Vec<String>,
    pub input_content_sha: String,
    pub extractive: Vec<ExtractiveSpan>,
    pub abstractive: crate::conversations::summarize::abstractive::AbstractiveResult,
}

pub async fn write_rollup(
    doc: &RollupDoc,
    narrative_embedding: Vec<f32>,
    force: bool,
    root_override: Option<&str>,
) -> Result<WriteResult> {
    use crate::conversations::summarize::abstractive::RollupKind;
    use chrono::TimeZone;

    let (md_path, history_dir, (synth_source, synth_conv, row_id, row_layer)) = match doc.kind {
        RollupKind::Week => (
            crate::conversations::paths::weekly_summary_path_for(&doc.window_label, root_override),
            crate::conversations::paths::weekly_history_dir(root_override),
            (
                "week",
                format!("week:{}", doc.window_label),
                format!("wk_{}_L3_0", doc.window_label),
                3i8,
            ),
        ),
        RollupKind::Month => (
            crate::conversations::paths::monthly_summary_path_for(&doc.window_label, root_override),
            crate::conversations::paths::monthly_history_dir(root_override),
            (
                "month",
                format!("month:{}", doc.window_label),
                format!("mo_{}_L4_0", doc.window_label),
                4i8,
            ),
        ),
    };

    if let Some(parent) = md_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let new_body = render_rollup(doc);

    let prior_exists = md_path.exists();
    let archived;
    let noop;

    if prior_exists {
        let existing = std::fs::read_to_string(&md_path)?;
        // Byte-equality short-circuit is a best-effort optimization for the
        // "rerun with same inputs" path. `--force` bypasses it — users who
        // pass --force explicitly want a fresh archive + rewrite regardless
        // of whether the body happens to be byte-identical (e.g. two runs
        // in the same wall-clock second producing the same generated_at).
        if !force && existing == new_body {
            return Ok(WriteResult {
                path: md_path,
                archived: None,
                noop: true,
            });
        }
        archived = Some(archive_prior_rollup(&md_path, &history_dir)?);
        // Phase 2C prune pattern — reuse history_retain from global config
        let retain = crate::store::config::load_config()
            .map(|c| c.conversations.compact.history_retain)
            .unwrap_or(5);
        let _ = prune_history_in(&history_dir, &doc.window_label, retain);
        noop = false;
    } else {
        archived = None;
        noop = false;
    }

    let tmp = md_path.with_file_name(format!(".tmp.{}.md", doc.window_label));
    let mut f = std::fs::File::create(&tmp).with_context(|| format!("open tmp {tmp:?}"))?;
    f.write_all(new_body.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &md_path)?;

    // Audit
    let mut h = Sha256::new();
    h.update(new_body.as_bytes());
    let content_sha = hex::encode(h.finalize());
    let audit_log = audit::Audit::open(root_override)?;
    audit_log.append(
        audit::AuditAction::Rollup {
            rollup_kind: doc.kind.as_str().to_string(),
            window: doc.window_label.clone(),
            model: doc.abstractive_model.clone(),
            duration_ms: doc.duration_ms,
        },
        content_sha,
    )?;

    // LanceDB single-row upsert
    let content_text = doc
        .abstractive
        .narrative
        .clone()
        .unwrap_or_else(|| "(rollup narrative unavailable)".to_string());
    let row_ts = chrono::Utc
        .from_utc_datetime(&doc.window_start.and_hms_opt(0, 0, 0).unwrap())
        .timestamp();
    let mut idx = crate::conversations::index::ConversationIndex::open(
        narrative_embedding.len() as i32,
        root_override,
    )
    .await?;
    idx.upsert_rollup_row(crate::conversations::index::RollupRow {
        id: &row_id,
        ts: row_ts,
        source: synth_source,
        conv_id: &synth_conv,
        layer: row_layer,
        content: &content_text,
        vector: &narrative_embedding,
    })
    .await?;

    Ok(WriteResult {
        path: md_path,
        archived,
        noop,
    })
}

fn archive_prior_rollup(md_path: &Path, history_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(history_dir)?;
    let stem = md_path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("stem")?;
    let now = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let dest = history_dir.join(format!("{stem}.{now}.md"));
    std::fs::rename(md_path, &dest)
        .with_context(|| format!("archive rollup {md_path:?} → {dest:?}"))?;
    Ok(dest)
}

fn prune_history_in(history_dir: &Path, window_label: &str, retain: u32) -> Result<u64> {
    if !history_dir.exists() {
        return Ok(0);
    }
    let mut matches: Vec<std::path::PathBuf> = std::fs::read_dir(history_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(window_label))
                .unwrap_or(false)
        })
        .collect();
    if matches.len() <= retain as usize {
        return Ok(0);
    }
    matches.sort();
    let drop_count = matches.len() - retain as usize;
    let mut freed = 0u64;
    for p in matches.into_iter().take(drop_count) {
        if let Ok(meta) = std::fs::metadata(&p) {
            freed += meta.len();
        }
        std::fs::remove_file(&p)?;
    }
    Ok(freed)
}

fn render_rollup(doc: &RollupDoc) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("schema: 1\n");
    out.push_str(&format!("kind: {}\n", doc.kind.as_str()));
    out.push_str(&format!("window: {}\n", doc.window_label));
    out.push_str(&format!("date: {}\n", doc.window_start));
    out.push_str(&format!(
        "source_labels: [{}]\n",
        doc.source_labels.join(", ")
    ));
    out.push_str(&format!(
        "generated_at: {}\n",
        doc.generated_at.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    out.push_str("generated_by:\n");
    out.push_str(&format!("  extractive_model: {}\n", doc.extractive_model));
    out.push_str(&format!("  abstractive_model: {}\n", doc.abstractive_model));
    out.push_str(&format!("  mur_version: {}\n", doc.mur_version));
    out.push_str(&format!("duration_ms: {}\n", doc.duration_ms));
    out.push_str(&format!("sources: [{}]\n", doc.sources.join(", ")));
    if doc.pattern_refs.is_empty() {
        out.push_str("pattern_refs: []\n");
    } else {
        out.push_str("pattern_refs:\n");
        for r in &doc.pattern_refs {
            out.push_str(&format!(
                "  - name: {}\n    version: {}\n    sha: {}\n",
                r.name, r.pattern_version, r.pattern_sha
            ));
        }
    }
    out.push_str(&format!("keywords: [{}]\n", doc.keywords.join(", ")));
    out.push_str("links:\n");
    out.push_str(&format!(
        "  prev: {}\n",
        doc.links_prev.as_deref().unwrap_or("null")
    ));
    out.push_str(&format!(
        "  next: {}\n",
        doc.links_next.as_deref().unwrap_or("null")
    ));
    if doc.warnings.is_empty() {
        out.push_str("warnings: []\n");
    } else {
        out.push_str("warnings:\n");
        for w in &doc.warnings {
            out.push_str(&format!("  - {}\n", w));
        }
    }
    out.push_str(&format!("input_content_sha: {}\n", doc.input_content_sha));
    out.push_str("---\n\n");

    out.push_str("## Extractive spans\n\n");
    for (i, s) in doc.extractive.iter().enumerate() {
        out.push_str(&format!(
            "[{}] _{{{}/{} @L{}}}_:\n> {}\n\n",
            i + 1,
            s.src.file_prefix(),
            s.conv_id,
            s.line_hint,
            s.text.replace('\n', "\n> ")
        ));
    }

    out.push_str("## Abstractive narrative\n\n");
    let narrative = doc
        .abstractive
        .narrative
        .as_deref()
        .unwrap_or("(rollup narrative generation failed; see warnings)");
    out.push_str(narrative);
    out.push_str("\n\n");

    if !doc.pattern_refs.is_empty() {
        out.push_str("## Macro expansion map\n\n");
        for r in &doc.pattern_refs {
            out.push_str(&format!(
                "- {} → patterns/{}.yaml (v{}, sha {}…)\n",
                r.marker,
                r.name,
                r.pattern_version,
                r.pattern_sha.chars().take(8).collect::<String>()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests;
