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
        if existing == new_body {
            return Ok(WriteResult {
                path: md_path,
                archived: None,
                noop: true,
            });
        }
        archived = Some(archive_prior(&md_path, root_override)?);
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let r = write_summary(&doc, vec![0.0; 16], Some(root))
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
        let _ = write_summary(&doc, vec![0.0; 16], Some(root))
            .await
            .unwrap();
        let r2 = write_summary(&d2, vec![0.0; 16], Some(root)).await.unwrap();
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
        let _ = write_summary(&doc1, vec![0.0; 16], Some(root))
            .await
            .unwrap();
        let mut doc2 = dummy_doc(date);
        doc2.abstractive.narrative = Some("version 2".into());
        let r2 = write_summary(&doc2, vec![0.0; 16], Some(root))
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
        let _ = write_summary(&doc, vec![0.0; 16], Some(root))
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
}
