//! Aggregate `Discovery` results across all detected runtimes, intersect
//! with the static preference table, and produce a ranked menu seed.

use std::cmp::Reverse;
use std::collections::HashSet;

use super::preference::{rank, EMBEDDING_PREFERENCE, LLM_PREFERENCE};
use super::{DiscoveredModel, ModelKind};

/// One menu row, in display order.
#[derive(Debug, Clone)]
pub struct MenuRow {
    pub kind: MenuRowKind,
    pub label: String,
    pub model: Option<DiscoveredModel>,
    pub pull_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuRowKind {
    Auto,   // first row, "[auto]" prefix
    Pulled, // already-installed model
    Pull,   // recommended-but-not-pulled
    Skip,   // last row
}

/// Build the menu rows for embedding selection.
///
/// `available` = union of all discovery results (filtered to kind ∈
/// {Embedding, Unknown}).
pub fn build_embedding_menu(available: &[DiscoveredModel]) -> Vec<MenuRow> {
    build_menu(available, EMBEDDING_PREFERENCE, ModelKind::Embedding)
}

/// Build the menu rows for LLM selection.
///
/// `available` = union of all discovery results (filtered to kind ∈
/// {Llm, Unknown}).
pub fn build_llm_menu(available: &[DiscoveredModel]) -> Vec<MenuRow> {
    build_menu(available, LLM_PREFERENCE, ModelKind::Llm)
}

fn build_menu(
    available: &[DiscoveredModel],
    table: &[(&'static str, u32)],
    desired_kind: ModelKind,
) -> Vec<MenuRow> {
    let mut filtered: Vec<&DiscoveredModel> = available
        .iter()
        .filter(|m| m.kind == desired_kind || m.kind == ModelKind::Unknown)
        .collect();
    // Stable sort: highest-ranked first. Tie-breaking preserves insertion order.
    filtered.sort_by_key(|m| Reverse(rank(&m.id, table)));

    let mut rows = Vec::new();

    // Row 1: [auto] = highest-ranked pulled model
    if let Some(top) = filtered.first() {
        let label = format!(
            "[auto] {}/{}{}",
            top.backend,
            top.id,
            top.dims.map(|d| format!(" ({}d)", d)).unwrap_or_default(),
        );
        rows.push(MenuRow {
            kind: MenuRowKind::Auto,
            label,
            model: Some((*top).clone()),
            pull_id: None,
        });
    }

    // Rows 2..: remaining pulled models
    for m in filtered.iter().skip(1) {
        let label = format!(
            "{}/{}{}",
            m.backend,
            m.id,
            m.dims.map(|d| format!(" ({}d)", d)).unwrap_or_default(),
        );
        rows.push(MenuRow {
            kind: MenuRowKind::Pulled,
            label,
            model: Some((*m).clone()),
            pull_id: None,
        });
    }

    // Top 2 preference-table entries NOT already in pulled set, with rank > 0.
    // Note: dedup uses substring `contains`, so a preference-table prefix like
    // "qwen3-embedding:0.6b" suppresses any pulled id that contains that
    // exact substring. If a future preference entry uses a very short prefix
    // (e.g. just "qwen3"), it would suppress unrelated families — keep
    // preference prefixes specific.
    let pulled_ids: HashSet<&str> = available.iter().map(|m| m.id.as_str()).collect();
    let mut suggestions: Vec<(&str, u32)> = table
        .iter()
        .filter(|(prefix, _)| !pulled_ids.iter().any(|id| id.contains(prefix)))
        .map(|(p, s)| (*p, *s))
        .collect();
    suggestions.sort_by_key(|(_, s)| Reverse(*s));
    for (prefix, _) in suggestions.iter().take(2) {
        rows.push(MenuRow {
            kind: MenuRowKind::Pull,
            label: format!("[pull] {}", prefix),
            model: None,
            pull_id: Some((*prefix).into()),
        });
    }

    // Last row: skip
    rows.push(MenuRow {
        kind: MenuRowKind::Skip,
        label: "Skip \u{2014} configure later".into(),
        model: None,
        pull_id: None,
    });

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Backend;

    fn dm(id: &str, backend: Backend, kind: ModelKind, dims: Option<usize>) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            backend,
            kind,
            dims,
            family: None,
            size_bytes: None,
            probed_at: None,
        }
    }

    #[test]
    fn empty_input_yields_pull_suggestions_then_skip() {
        let rows = build_embedding_menu(&[]);
        // 0 auto + 0 pulled + 2 [pull] + 1 skip
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].kind, MenuRowKind::Pull);
        assert_eq!(rows.last().unwrap().kind, MenuRowKind::Skip);
    }

    #[test]
    fn single_pulled_becomes_auto() {
        let avail =
            vec![dm("qwen3-embedding:0.6b", Backend::Ollama, ModelKind::Embedding, Some(1024))];
        let rows = build_embedding_menu(&avail);
        assert_eq!(rows[0].kind, MenuRowKind::Auto);
        assert!(rows[0].label.starts_with("[auto] Ollama/qwen3-embedding:0.6b"));
        assert!(rows[0].label.contains("(1024d)"));
    }

    #[test]
    fn omlx_and_ollama_both_present_one_is_auto() {
        // Both have rank 70; we just assert one of them is [auto], the other [Pulled].
        let avail = vec![
            dm("qwen3-embedding:0.6b", Backend::Ollama, ModelKind::Embedding, Some(1024)),
            dm(
                "mlx-community/Qwen3-Embedding-0.6B-8bit",
                Backend::OMlx,
                ModelKind::Embedding,
                Some(1024),
            ),
        ];
        let rows = build_embedding_menu(&avail);
        assert_eq!(rows[0].kind, MenuRowKind::Auto);
        assert_eq!(rows[1].kind, MenuRowKind::Pulled);
    }

    #[test]
    fn pull_suggestion_excludes_already_pulled() {
        let avail =
            vec![dm("qwen3-embedding:0.6b", Backend::Ollama, ModelKind::Embedding, Some(1024))];
        let rows = build_embedding_menu(&avail);
        for r in &rows {
            if let Some(pid) = &r.pull_id {
                assert!(!pid.contains("qwen3-embedding:0.6b"));
            }
        }
    }

    #[test]
    fn unknown_kind_included_in_filter() {
        let avail = vec![dm("foo:bar", Backend::Ollama, ModelKind::Unknown, None)];
        let rows = build_embedding_menu(&avail);
        // Unknown is included in the embedding filter
        assert_eq!(rows[0].kind, MenuRowKind::Auto);
    }

    #[test]
    fn llm_kind_not_in_embedding_menu() {
        let avail = vec![dm("qwen3.5:4b", Backend::Ollama, ModelKind::Llm, None)];
        let rows = build_embedding_menu(&avail);
        // llm-only entries must NOT appear as pulled/auto
        assert!(rows
            .iter()
            .all(|r| r.model.as_ref().map(|m| m.kind) != Some(ModelKind::Llm)));
    }
}
