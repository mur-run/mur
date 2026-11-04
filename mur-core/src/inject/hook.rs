//! Hook/inject: format skills and workflows for injection into AI tool prompts.
//!
//! ## Post-session feedback integration
//!
//! After injection, we write `~/.mur/last_injection.json` so that
//! `mur feedback auto` can analyze the session transcript and update
//! pattern confidence. Claude Code hooks can trigger this automatically:
//!
//! ```json
//! // .claude/hooks.json (future integration)
//! {
//!   "post_session": [{
//!     "command": "mur feedback auto --file /path/to/transcript"
//!   }]
//! }
//! ```
//!
// TODO(phase-2): Integrate with Claude Code post-session hooks when the
// hooks API supports session-end events. For now, `mur feedback auto`
// is run manually or via shell scripts.

use mur_common::workflow::Workflow;

use crate::capture::feedback::{InjectedItemRecord, InjectionRecord, write_injection_record};
use crate::evolve::cooccurrence::CooccurrenceMatrix;

// ── InjectedItem: decoupled injection representation (Pattern→Skill migration) ─

/// Decoupled representation carrying everything the injection formatters need.
/// Bridges `LoadedSkill` (and future Workflow items) into a single formatting
/// pipeline so formatters don't depend on the storage type.
#[derive(Debug, Clone)]
pub struct InjectedItem {
    pub name: String,
    pub description: String,
    pub content_text: String,
    pub kind_group: KindGroup,
}

/// When to trigger pattern retrieval
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // Manual variant used by CLI callers
pub enum HookTrigger {
    /// Beginning of AI session
    SessionStart,
    /// Error/failure detected in session
    OnError,
    /// Repeated attempt detected
    OnRetry,
    /// Manual: `mur inject --query "..."`
    Manual,
}

/// Detect what kind of hook trigger a message represents.
#[allow(dead_code)] // M4 will reintroduce per-tier trigger behavior
pub fn detect_trigger(message: &str) -> HookTrigger {
    let lower = message.to_lowercase();

    // Error indicators
    let error_keywords = [
        "error",
        "fail",
        "crash",
        "panic",
        "exception",
        "traceback",
        "segfault",
        "abort",
        "undefined",
        "cannot find",
        "not found",
        "permission denied",
        "timeout",
        "refused",
        "broken",
        "錯誤",
        "失敗",
        "崩潰",
    ];
    for kw in &error_keywords {
        if lower.contains(kw) {
            return HookTrigger::OnError;
        }
    }

    // Retry indicators
    let retry_keywords = [
        "again",
        "retry",
        "try again",
        "still not",
        "same error",
        "still failing",
        "didn't work",
        "not working",
        "再試",
        "還是不行",
        "一樣的問題",
    ];
    for kw in &retry_keywords {
        if lower.contains(kw) {
            return HookTrigger::OnRetry;
        }
    }

    HookTrigger::SessionStart
}

/// Kind category for grouping injection output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KindGroup {
    Preferences,
    Procedures,
    Knowledge,
}

impl KindGroup {
    fn header(&self) -> &'static str {
        match self {
            KindGroup::Preferences => "## User Preferences",
            KindGroup::Procedures => "## Procedures",
            KindGroup::Knowledge => "## Knowledge",
        }
    }
}

/// Format a single workflow entry for injection.
pub fn format_workflow_entry(workflow: &Workflow, index: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "### {}. [Workflow: {}] {}\n",
        index, workflow.name, workflow.description
    ));
    s.push_str("**Follow this workflow if the task matches.** ");
    s.push_str("Run `mur workflow show ");
    s.push_str(&workflow.name);
    s.push_str(" --md` for full details.\n");

    // Variables
    if !workflow.variables.is_empty() {
        s.push_str("Variables: ");
        let vars: Vec<String> = workflow
            .variables
            .iter()
            .map(|v| {
                let default = v.default.as_deref().unwrap_or("?");
                format!("`{}`={}", v.name, default)
            })
            .collect();
        s.push_str(&vars.join(", "));
        s.push('\n');
    }

    // Tools
    if !workflow.tools.is_empty() {
        s.push_str(&format!("Tools: {}\n", workflow.tools.join(", ")));
    }

    // Steps summary
    if !workflow.steps.is_empty() {
        s.push_str("Steps:\n");
        for step in &workflow.steps {
            s.push_str(&format!("  {}. {}", step.order, step.description));
            if let Some(cmd) = &step.command {
                s.push_str(&format!(" (`{}`)", cmd));
            }
            s.push('\n');
        }
    }

    s.push('\n');
    s
}

/// Format scored skills and workflows for injection. Mirrors
/// the removed pattern formatter, but for the skill corpus.
pub fn format_skills_for_injection(
    skills: &[crate::retrieve::scoring::Scored<crate::retrieve::skill_candidates::LoadedSkill>],
    workflows: &[Workflow],
    max_tokens: usize,
) -> String {
    if skills.is_empty() && workflows.is_empty() {
        return String::new();
    }
    let mut output = String::from("## Relevant skills from your learning history\n\n");
    let mut token_count = output.len() / 4;
    let mut index = 1;
    for scored in skills {
        let s = &scored.item.manifest;
        let body = match &s.content.context {
            Some(ctx) if !ctx.is_empty() => format!("{}\n\n{}", s.content.r#abstract, ctx),
            _ => s.content.r#abstract.clone(),
        };
        let entry = format!("### {}. {}\n{}\n", index, s.description, body.trim());
        let entry_tokens = entry.len() / 4;
        if token_count + entry_tokens > max_tokens && index > 1 {
            break;
        }
        output.push_str(&entry);
        output.push('\n');
        token_count += entry_tokens;
        index += 1;
    }
    for workflow in workflows {
        let entry = format_workflow_entry(workflow, index);
        let entry_tokens = entry.len() / 4;
        if token_count + entry_tokens > max_tokens && index > 1 {
            break;
        }
        output.push_str(&entry);
        token_count += entry_tokens;
        index += 1;
    }
    output.trim_end().to_string()
}

// ── Item-based formatters (Skill pathway) ────────────────────────────

/// Format scored items for injection (Skill pathway).
pub fn format_for_injection_items(items: &[InjectedItem], max_tokens: usize) -> String {
    if items.is_empty() {
        return String::new();
    }
    let all_knowledge = items
        .iter()
        .all(|i| matches!(i.kind_group, KindGroup::Knowledge));
    if all_knowledge {
        return format_flat_injection_items(items, max_tokens);
    }
    format_grouped_injection_items(items, max_tokens)
}

fn format_flat_injection_items(items: &[InjectedItem], max_tokens: usize) -> String {
    let mut output = String::from("## Relevant patterns from your learning history\n\n");
    let mut token_count = output.len() / 4;
    for (i, item) in items.iter().enumerate() {
        let entry = format!(
            "### {}. {}\n{}\n",
            i + 1,
            item.description,
            item.content_text.trim()
        );
        let entry_tokens = entry.len() / 4;
        if token_count + entry_tokens > max_tokens && i > 0 {
            break;
        }
        output.push_str(&entry);
        output.push('\n');
        token_count += entry_tokens;
    }
    output.trim_end().to_string()
}

fn format_grouped_injection_items(items: &[InjectedItem], max_tokens: usize) -> String {
    let mut output = String::from("## Relevant knowledge from your learning history\n\n");
    let mut token_count = output.len() / 4;
    let group_order = [
        KindGroup::Preferences,
        KindGroup::Procedures,
        KindGroup::Knowledge,
    ];
    let mut index = 1;
    'outer: for group in &group_order {
        let group_items: Vec<&InjectedItem> =
            items.iter().filter(|i| i.kind_group == *group).collect();
        if group_items.is_empty() {
            continue;
        }
        let group_header = format!("{}\n\n", group.header());
        let mut group_buf = group_header;
        let mut buf_tokens = group_buf.len() / 4;
        let mut any_added = false;
        for item in group_items {
            let entry = match group {
                KindGroup::Preferences => {
                    format!("- **{}**: {}\n", item.description, item.content_text.trim())
                }
                KindGroup::Procedures => {
                    format!("### {}\n{}\n", item.description, item.content_text.trim())
                }
                KindGroup::Knowledge => format!(
                    "### {}. {}\n{}\n",
                    index,
                    item.description,
                    item.content_text.trim()
                ),
            };
            let entry_tokens = entry.len() / 4;
            let is_global_first = index == 1 && !any_added;
            if !is_global_first && token_count + buf_tokens + entry_tokens > max_tokens {
                if any_added {
                    output.push_str(&group_buf);
                }
                break 'outer;
            }
            group_buf.push_str(&entry);
            group_buf.push('\n');
            buf_tokens += entry_tokens;
            index += 1;
            any_added = true;
        }
        if any_added {
            output.push_str(&group_buf);
            token_count += buf_tokens;
        } else {
            break 'outer;
        }
    }
    output.trim_end().to_string()
}

/// Format both items and workflows for unified injection (Skill pathway).
pub fn format_unified_injection_items(
    items: &[InjectedItem],
    workflows: &[Workflow],
    max_tokens: usize,
) -> String {
    if items.is_empty() && workflows.is_empty() {
        return String::new();
    }
    if workflows.is_empty() {
        return format_for_injection_items(items, max_tokens);
    }
    let mut output = String::from("## Relevant knowledge from your learning history\n\n");
    let mut token_count = output.len() / 4;
    let mut index = 1;
    for item in items {
        let entry = format!(
            "### {}. {}\n{}\n",
            index,
            item.description,
            item.content_text.trim()
        );
        let entry_tokens = entry.len() / 4;
        if token_count + entry_tokens > max_tokens && index > 1 {
            break;
        }
        output.push_str(&entry);
        output.push('\n');
        token_count += entry_tokens;
        index += 1;
    }
    for workflow in workflows {
        let entry = format_workflow_entry(workflow, index);
        let entry_tokens = entry.len() / 4;
        if token_count + entry_tokens > max_tokens && index > 1 {
            break;
        }
        output.push_str(&entry);
        token_count += entry_tokens;
        index += 1;
    }
    output.trim_end().to_string()
}

/// Record injected items (Skill pathway).
pub fn record_injection_items(query: &str, project: &str, items: &[InjectedItem]) {
    let records: Vec<InjectedItemRecord> = items
        .iter()
        .map(|item| {
            let snippet = if item.content_text.len() > 100 {
                format!("{}...", &item.content_text[..100])
            } else {
                item.content_text.clone()
            };
            InjectedItemRecord {
                name: item.name.clone(),
                snippet,
            }
        })
        .collect();
    let record = InjectionRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        query: query.to_string(),
        project: project.to_string(),
        patterns: records,
    };
    if let Err(e) = write_injection_record(&record) {
        eprintln!("# Warning: failed to write injection record: {}", e);
    }
}

/// Record co-occurrence for items (Skill pathway).
pub fn record_cooccurrence_for_items(items: &[InjectedItem]) {
    if items.len() < 2 {
        return;
    }
    let path = CooccurrenceMatrix::default_path();
    let mut matrix = match CooccurrenceMatrix::load(&path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("# Warning: failed to load cooccurrence matrix: {}", e);
            CooccurrenceMatrix::new()
        }
    };
    let names: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
    matrix.record_cooccurrence(&names);
    if let Err(e) = matrix.save(&path) {
        eprintln!("# Warning: failed to save cooccurrence matrix: {}", e);
    }
}

// ---------- P1.3: format external-source notes ----------

#[cfg(feature = "sources")]
#[allow(dead_code)] // called by CLI inject path wired in P1.4
pub fn format_notes_section(hits: &[crate::store::vector::Hit]) -> String {
    if hits.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&format!("\n## Notes ({})\n\n", hits.len()));
    for h in hits {
        let hp = if h.heading_path.is_empty() {
            String::new()
        } else {
            format!(" § \"{}\"", h.heading_path.join(" / "))
        };
        out.push_str(&format!(
            "[Note: {} / {}{}]\n",
            h.source_id, h.external_id, hp
        ));
        let preview: String = h.text.chars().take(400).collect();
        out.push_str("  ");
        out.push_str(&preview);
        if h.text.chars().count() > 400 {
            out.push_str("...");
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

/// Fetch source hits for a query from the vector + BM25 index.
///
/// Returns up to `k` hits from the unified retrieve pipeline. All errors
/// are swallowed (returns empty vec) so injection never fails due to sources.
///
/// Wiring into the main inject entry is deferred: the top-level inject
/// functions (`format_for_injection_with_store`)
/// are synchronous, and making them async touches several callers across the
/// codebase. The recommended approach for a follow-up task is to either:
///   (a) make the inject entry async and call this directly, or
///   (b) resolve the hits in the async CLI handler and pass them in as a slice.
#[cfg(feature = "sources")]
#[allow(dead_code)] // wiring deferred: called from inject entry point in P1.5
pub async fn fetch_source_hits_for_query(query: &str, k: usize) -> Vec<crate::store::vector::Hit> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::{SearchFilter, factory::get_vector_store};

    let Ok(cfg) = crate::store::config::load_config() else {
        return vec![];
    };
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let Some(mur_dir) = mur_common::home::try_mur_home() else {
        return vec![];
    };
    let index_path = mur_dir.join("index");
    let Ok(vs) = get_vector_store(&cfg, &index_path).await else {
        return vec![];
    };
    let Ok(tantivy) = crate::sources::tantivy::TantivyIndex::open_or_create(&mur_dir) else {
        return vec![];
    };
    let weights: std::collections::HashMap<String, f32> =
        crate::sources::instance::SourceInstanceStore::default_store()
            .and_then(|s| s.list())
            .map(|v| v.into_iter().map(|i| (i.id, i.weight)).collect())
            .unwrap_or_default();
    let Ok(_qvec) = embed(query, &emb_cfg).await else {
        return vec![];
    };
    let filter = SearchFilter::default();
    let Ok(unified) = crate::retrieve::retrieve_unified(
        query, vs, &tantivy, &emb_cfg, &weights, &filter, 0, k, 0.35,
    )
    .await
    else {
        return vec![];
    };
    unified.into_iter().map(|u| u.hit).collect()
}

#[cfg(all(test, feature = "sources"))]
mod notes_tests {
    use super::*;
    use crate::store::vector::Hit;

    #[test]
    fn empty_hits_yields_empty_string() {
        assert!(format_notes_section(&[]).is_empty());
    }

    #[test]
    fn renders_source_id_and_heading_path() {
        let hit = Hit {
            chunk_id: "c".into(),
            source_id: "obsidian:main".into(),
            external_id: "design/auth.md".into(),
            score: 0.9,
            text: "JWT 15min + 7d refresh.".into(),
            heading_path: vec!["Design".into(), "Auth".into()],
            updated_at: chrono::Utc::now(),
        };
        let out = format_notes_section(&[hit]);
        assert!(out.contains("## Notes (1)"));
        assert!(out.contains("obsidian:main / design/auth.md"));
        assert!(out.contains("Design / Auth"));
    }

    #[test]
    fn long_text_gets_truncated() {
        let big = "x".repeat(1000);
        let hit = Hit {
            chunk_id: "c".into(),
            source_id: "s".into(),
            external_id: "d".into(),
            score: 0.5,
            text: big,
            heading_path: vec![],
            updated_at: chrono::Utc::now(),
        };
        let out = format_notes_section(&[hit]);
        assert!(out.contains("..."));
    }
}

#[cfg(test)]
mod tests;
