//! Pattern → Workflow composition suggestions.
//!
//! Analyzes the co-occurrence matrix to suggest workflows that bundle
//! frequently co-occurring patterns. Only suggests when 3+ patterns
//! co-occur 5+ times (configurable thresholds).

use mur_common::pattern::Pattern;

use super::cooccurrence::CooccurrenceMatrix;

/// A suggestion to create a workflow from co-occurring patterns.
#[derive(Debug, Clone)]
pub struct WorkflowSuggestion {
    /// Pattern names that form this suggestion
    pub patterns: Vec<String>,
    /// Auto-generated workflow name
    pub suggested_name: String,
    /// Auto-generated trigger description
    pub suggested_trigger: String,
    /// Total co-occurrence score
    pub cooccurrence_score: u32,
}

/// Suggest workflows from co-occurrence data.
///
/// Only suggests when `min_patterns`+ patterns co-occur `threshold`+ times.
/// Default: 3+ patterns, 5+ co-occurrences.
#[allow(dead_code)] // Used by tests
pub fn suggest_workflows(matrix: &CooccurrenceMatrix, threshold: u32) -> Vec<WorkflowSuggestion> {
    suggest_workflows_with_patterns(matrix, threshold, &[])
}

/// Suggest workflows with access to full pattern data for richer trigger generation.
pub fn suggest_workflows_with_patterns(
    matrix: &CooccurrenceMatrix,
    threshold: u32,
    patterns: &[Pattern],
) -> Vec<WorkflowSuggestion> {
    let clusters = matrix.find_clusters(threshold);
    let min_patterns = 3;

    clusters
        .into_iter()
        .filter(|cluster| cluster.pattern_names.len() >= min_patterns)
        .map(|cluster| {
            let trigger = generate_trigger(&cluster.pattern_names, patterns);
            WorkflowSuggestion {
                suggested_name: cluster.suggested_workflow_name,
                suggested_trigger: trigger,
                cooccurrence_score: cluster.total_cooccurrences,
                patterns: cluster.pattern_names,
            }
        })
        .collect()
}

/// Generate a trigger description from pattern metadata.
///
/// Looks at common tags and applies fields to produce something like
/// "when working with rust error handling and testing".
fn generate_trigger(pattern_names: &[String], patterns: &[Pattern]) -> String {
    if patterns.is_empty() {
        return format!("when using {} together", pattern_names.join(", "));
    }

    // Collect tags from matching patterns
    let mut tag_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut languages: Vec<&str> = Vec::new();

    for name in pattern_names {
        if let Some(p) = patterns.iter().find(|p| &p.name == name) {
            for topic in &p.tags.topics {
                *tag_counts.entry(topic.as_str()).or_insert(0) += 1;
            }
            for lang in &p.tags.languages {
                if !languages.contains(&lang.as_str()) {
                    languages.push(lang.as_str());
                }
            }
        }
    }

    // Find tags that appear in most patterns (> 50%)
    let half = pattern_names.len() / 2;
    let mut common_tags: Vec<&str> = tag_counts
        .iter()
        .filter(|(_, count)| **count > half)
        .map(|(tag, _)| *tag)
        .collect();
    common_tags.sort();

    let mut parts = Vec::new();
    if !languages.is_empty() {
        parts.push(languages.join("/"));
    }
    if !common_tags.is_empty() {
        parts.push(common_tags.join(" and "));
    }

    if parts.is_empty() {
        format!("when using {} together", pattern_names.join(", "))
    } else {
        format!("when working with {}", parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::*;

    fn make_pattern(name: &str, topics: Vec<&str>, languages: Vec<&str>) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: format!("Pattern: {}", name),
                content: Content::Plain(format!("Content for {}", name)),
                tags: Tags {
                    topics: topics.into_iter().map(String::from).collect(),
                    languages: languages.into_iter().map(String::from).collect(),
                    extra: Default::default(),
                },
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    #[test]
    fn test_suggest_workflows_basic() {
        let mut matrix = CooccurrenceMatrix::new();
        // 3 patterns co-occurring 6 times
        for _ in 0..6 {
            matrix.record_cooccurrence(&[
                "rust-errors".into(),
                "rust-testing".into(),
                "rust-logging".into(),
            ]);
        }

        let suggestions = suggest_workflows(&matrix, 5);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].patterns.len(), 3);
        assert!(suggestions[0].cooccurrence_score >= 15);
        assert!(suggestions[0].suggested_name.contains("rust"));
    }

    #[test]
    fn test_suggest_workflows_below_min_patterns() {
        let mut matrix = CooccurrenceMatrix::new();
        // Only 2 patterns — below min_patterns threshold of 3
        for _ in 0..10 {
            matrix.record_cooccurrence(&["a".into(), "b".into()]);
        }

        let suggestions = suggest_workflows(&matrix, 5);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_suggest_workflows_below_cooccurrence_threshold() {
        let mut matrix = CooccurrenceMatrix::new();
        // Only 2 co-occurrences — below threshold
        for _ in 0..2 {
            matrix.record_cooccurrence(&["a".into(), "b".into(), "c".into()]);
        }

        let suggestions = suggest_workflows(&matrix, 5);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_suggest_with_pattern_metadata() {
        let mut matrix = CooccurrenceMatrix::new();
        for _ in 0..6 {
            matrix.record_cooccurrence(&[
                "rust-errors".into(),
                "rust-testing".into(),
                "rust-logging".into(),
            ]);
        }

        let patterns = vec![
            make_pattern("rust-errors", vec!["errors", "handling"], vec!["rust"]),
            make_pattern("rust-testing", vec!["testing", "errors"], vec!["rust"]),
            make_pattern("rust-logging", vec!["logging", "errors"], vec!["rust"]),
        ];

        let suggestions = suggest_workflows_with_patterns(&matrix, 5, &patterns);
        assert_eq!(suggestions.len(), 1);
        let trigger = &suggestions[0].suggested_trigger;
        assert!(
            trigger.contains("rust"),
            "trigger should mention rust: {}",
            trigger
        );
    }

    #[test]
    fn test_generate_trigger_no_patterns() {
        let names = vec!["a".to_string(), "b".to_string()];
        let trigger = generate_trigger(&names, &[]);
        assert!(trigger.contains("a, b"));
    }

    #[test]
    fn test_generate_trigger_with_common_tags() {
        let patterns = vec![
            make_pattern("p1", vec!["testing", "ci"], vec!["rust"]),
            make_pattern("p2", vec!["testing", "ci"], vec!["rust"]),
            make_pattern("p3", vec!["testing", "deploy"], vec!["rust"]),
        ];
        let names: Vec<String> = patterns.iter().map(|p| p.name.clone()).collect();
        let trigger = generate_trigger(&names, &patterns);
        assert!(
            trigger.contains("rust"),
            "trigger should mention rust: {}",
            trigger
        );
        assert!(
            trigger.contains("testing"),
            "trigger should mention testing: {}",
            trigger
        );
    }
}
