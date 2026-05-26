//! Consolidation pass: dedup, contradiction, orphan detection (M5b + M6c.1).
//!
//! Runs three read-only scans and optionally (`--apply`) mutates state.
//! All findings are written to a JSONL report at
//! `<MUR_HOME>/skills/_consolidation/<date>.jsonl`.

use anyhow::Result;
use chrono::Utc;
use mur_common::skill::stats::SkillStats;
use std::path::{Path, PathBuf};

use crate::store::embedding::EmbeddingConfig;
use crate::store::vector::VectorStore;

pub mod contradiction;
pub mod contradiction_llm;
pub mod dedup;
pub mod dedup_vec;
pub mod orphan;

#[derive(Debug, Clone)]
pub enum ConsolidateMethod {
    Jaccard,
    Vector,
    Both,
}

impl serde::Serialize for ConsolidateMethod {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Jaccard => s.serialize_str("jaccard"),
            Self::Vector => s.serialize_str("vector"),
            Self::Both => s.serialize_str("both"),
        }
    }
}

pub struct ConsolidateOptions {
    #[allow(dead_code)]
    pub dry_run: bool,
    pub apply: bool,
    pub method: ConsolidateMethod,
    pub llm_adjudicate: bool,
}

#[derive(Debug)]
pub struct ConsolidateReport {
    pub method: ConsolidateMethod,
    pub duplicates: Vec<dedup::DuplicatePair>,
    pub contradictions: Vec<contradiction::ContradictionPair>,
    pub orphans: Vec<orphan::OrphanFinding>,
}

/// Minimal snapshot of a skill for consolidation scans.
pub struct SkillView {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub requires: Vec<String>,
    pub stats: SkillStats,
    /// Canonical text for embedding (same format as skill_index::text::embed_manifest).
    pub embed_text: String,
}

pub async fn run_consolidate(
    home: &Path,
    embed_config: &EmbeddingConfig,
    store: &dyn VectorStore,
    opts: &ConsolidateOptions,
) -> Result<ConsolidateReport> {
    let skills = load_all_with_stats(home)?;
    let mut report = ConsolidateReport {
        method: opts.method.clone(),
        duplicates: Vec::new(),
        contradictions: Vec::new(),
        orphans: Vec::new(),
    };

    match &opts.method {
        ConsolidateMethod::Jaccard => {
            dedup::scan(&skills, &mut report);
        }
        ConsolidateMethod::Vector => {
            dedup_vec::scan(&skills, embed_config, store, &mut report).await?;
        }
        ConsolidateMethod::Both => {
            dedup::scan(&skills, &mut report);
            dedup_vec::scan(&skills, embed_config, store, &mut report).await?;
        }
    }

    contradiction::scan(&skills, &mut report);
    orphan::scan(&skills, &mut report, Utc::now())?;

    // LLM adjudication (M6c Task 5)
    if opts.llm_adjudicate {
        contradiction_llm::adjudicate(&mut report.contradictions, &skills, home).await;
    }

    if opts.apply {
        apply_findings(home, &mut report)?;
    }

    write_jsonl_report(home, &report, opts.apply)?;
    Ok(report)
}

fn load_all_with_stats(home: &Path) -> Result<Vec<SkillView>> {
    let installed =
        mur_common::skill::local::list_installed(home).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut views = Vec::new();

    for name in installed {
        let stats_path = SkillStats::path(home, &name);
        let stats = SkillStats::load(&stats_path)?
            .unwrap_or_else(|| SkillStats::new(&name, "unknown", "", Utc::now()));

        // Load manifest for description/triggers/requires + embed text
        let manifest_path = home.join("skills").join(&name).join("skill.yaml");
        let (description, triggers, requires, embed_text) =
            if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                if let Ok(m) = mur_common::skill::parser::parse_canonical(&content) {
                    let et = crate::skill_index::text::embed_manifest(&m);
                    (
                        m.description,
                        m.triggers
                            .iter()
                            .filter_map(|t| t.pattern.clone())
                            .collect(),
                        m.requires.into_iter().map(|r| r.name).collect(),
                        et,
                    )
                } else {
                    (String::new(), vec![], vec![], String::new())
                }
            } else {
                (String::new(), vec![], vec![], String::new())
            };

        views.push(SkillView {
            name,
            description,
            triggers,
            requires,
            stats,
            embed_text,
        });
    }
    Ok(views)
}

fn apply_findings(home: &Path, report: &mut ConsolidateReport) -> Result<()> {
    for dup in &report.duplicates {
        // Flip loser to Deprecated with reason
        let path = SkillStats::path(home, &dup.b);
        let keeper = dup.keeper.clone();
        let reason = format!("duplicate_of:{keeper}");
        SkillStats::merge_in_place(
            &path,
            || SkillStats::new(&dup.b, "unknown", "", Utc::now()),
            |s| {
                s.lifecycle_state = mur_common::skill::stats::LifecycleState::Deprecated;
                s.lifecycle_changed_at = Utc::now();
                s.pinned_reason = reason;
                Ok(())
            },
        )?;
    }

    for orphan in &report.orphans {
        let path = SkillStats::path(home, &orphan.name);
        SkillStats::merge_in_place(
            &path,
            || SkillStats::new(&orphan.name, "unknown", "", Utc::now()),
            |s| {
                s.lifecycle_state = mur_common::skill::stats::LifecycleState::Archived;
                s.lifecycle_changed_at = Utc::now();
                s.pinned_reason = "archived: consolidate orphan".into();
                Ok(())
            },
        )?;
    }

    Ok(())
}

fn write_jsonl_report(home: &Path, report: &ConsolidateReport, applied: bool) -> Result<()> {
    let dir = home.join("skills").join("_consolidation");
    std::fs::create_dir_all(&dir)?;
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let path = dir.join(format!("{today}.jsonl"));

    let mut lines = Vec::new();

    // Header line with method + timestamp
    lines.push(serde_json::json!({
        "type": "header",
        "method": report.method,
        "started_at": Utc::now().to_rfc3339(),
    }));

    for d in &report.duplicates {
        lines.push(serde_json::json!({
            "type": "duplicate",
            "a": d.a,
            "b": d.b,
            "similarity": d.similarity,
            "keeper": d.keeper,
            "source": d.source,
            "applied": applied,
            "applied_at": Utc::now().to_rfc3339(),
        }));
    }
    for c in &report.contradictions {
        lines.push(serde_json::json!({
            "type": "contradiction",
            "a": c.a,
            "b": c.b,
            "trigger": c.trigger,
            "reason": c.reason,
            "adjudication": c.adjudication.as_ref().map(|v| v.as_str()),
            "applied": applied,
            "applied_at": Utc::now().to_rfc3339(),
        }));
    }
    for o in &report.orphans {
        lines.push(serde_json::json!({
            "type": "orphan",
            "name": o.name,
            "last_used": o.last_used.map(|t| t.to_rfc3339()),
            "usage_count": o.usage_count,
            "applied": applied,
            "applied_at": Utc::now().to_rfc3339(),
        }));
    }

    let content: String = lines
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    if !content.is_empty() {
        std::fs::write(&path, format!("{content}\n"))?;
    }
    Ok(())
}

/// Public path for use by the CLI dispatcher.
#[allow(dead_code)]
pub fn report_dir(home: &Path) -> PathBuf {
    home.join("skills").join("_consolidation")
}
