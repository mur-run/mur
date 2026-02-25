//! Co-occurrence matrix — track which patterns are injected together.
//!
//! When patterns are injected in the same session, their co-occurrence
//! count is incremented. Over time this reveals natural clusters of
//! patterns that tend to be used together, which can be composed into
//! workflow suggestions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A pair of pattern names (canonically ordered so (A,B) == (B,A)).
fn canonical_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// A group of patterns that frequently co-occur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternCluster {
    /// Pattern names in this cluster
    pub pattern_names: Vec<String>,
    /// Total co-occurrence count across all pairs in the cluster
    pub total_cooccurrences: u32,
    /// Auto-generated suggested workflow name
    pub suggested_workflow_name: String,
}

/// Serializable representation of the co-occurrence matrix.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CooccurrenceMatrix {
    /// Map from "patternA::patternB" → count (canonical order)
    pairs: HashMap<String, u32>,
}

impl CooccurrenceMatrix {
    /// Create a new empty matrix.
    pub fn new() -> Self {
        Self::default()
    }

    /// Encode a pair key for the HashMap.
    fn pair_key(a: &str, b: &str) -> String {
        let (x, y) = canonical_pair(a, b);
        format!("{}::{}", x, y)
    }

    /// Decode a pair key back into two pattern names.
    fn decode_pair_key(key: &str) -> Option<(String, String)> {
        let parts: Vec<&str> = key.splitn(2, "::").collect();
        if parts.len() == 2 {
            Some((parts[0].to_string(), parts[1].to_string()))
        } else {
            None
        }
    }

    /// Record co-occurrences for a set of patterns injected together.
    /// For N patterns, records N*(N-1)/2 pairs.
    pub fn record_cooccurrence(&mut self, injected_patterns: &[String]) {
        if injected_patterns.len() < 2 {
            return;
        }

        for i in 0..injected_patterns.len() {
            for j in (i + 1)..injected_patterns.len() {
                let key = Self::pair_key(&injected_patterns[i], &injected_patterns[j]);
                *self.pairs.entry(key).or_insert(0) += 1;
            }
        }
    }

    /// Get the co-occurrence count for a specific pair.
    pub fn get_count(&self, a: &str, b: &str) -> u32 {
        let key = Self::pair_key(a, b);
        self.pairs.get(&key).copied().unwrap_or(0)
    }

    /// Get all pairs with their counts.
    #[allow(dead_code)] // Public API
    pub fn all_pairs(&self) -> Vec<((String, String), u32)> {
        self.pairs
            .iter()
            .filter_map(|(key, &count)| {
                Self::decode_pair_key(key).map(|pair| (pair, count))
            })
            .collect()
    }

    /// Total number of tracked pairs.
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    /// Find clusters of patterns that frequently co-occur.
    ///
    /// Algorithm: greedy agglomerative clustering.
    /// 1. Start with all pairs above `min_cooccurrence`
    /// 2. Build adjacency from strong pairs
    /// 3. Find connected components as clusters
    pub fn find_clusters(&self, min_cooccurrence: u32) -> Vec<PatternCluster> {
        // Build adjacency list from pairs above threshold
        let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();

        for (key, &count) in &self.pairs {
            if count < min_cooccurrence {
                continue;
            }
            if let Some((a, b)) = Self::decode_pair_key(key) {
                adjacency.entry(a.clone()).or_default().push(b.clone());
                adjacency.entry(b.clone()).or_default().push(a.clone());
            }
        }

        if adjacency.is_empty() {
            return Vec::new();
        }

        // Find connected components via BFS
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut clusters = Vec::new();

        for start in adjacency.keys() {
            if visited.contains(start) {
                continue;
            }

            let mut component = Vec::new();
            let mut queue = std::collections::VecDeque::new();
            queue.push_back(start.clone());
            visited.insert(start.clone());

            while let Some(node) = queue.pop_front() {
                component.push(node.clone());
                if let Some(neighbors) = adjacency.get(&node) {
                    for neighbor in neighbors {
                        if !visited.contains(neighbor) {
                            visited.insert(neighbor.clone());
                            queue.push_back(neighbor.clone());
                        }
                    }
                }
            }

            if component.len() >= 2 {
                component.sort();

                // Sum co-occurrence counts within this cluster
                let mut total = 0u32;
                for i in 0..component.len() {
                    for j in (i + 1)..component.len() {
                        total += self.get_count(&component[i], &component[j]);
                    }
                }

                let suggested_name = generate_cluster_name(&component);

                clusters.push(PatternCluster {
                    pattern_names: component,
                    total_cooccurrences: total,
                    suggested_workflow_name: suggested_name,
                });
            }
        }

        // Sort clusters by total co-occurrences (highest first)
        clusters.sort_by(|a, b| b.total_cooccurrences.cmp(&a.total_cooccurrences));
        clusters
    }

    /// Default path: `~/.mur/cooccurrence.json`
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".mur")
            .join("cooccurrence.json")
    }

    /// Load from a JSON file. Returns empty matrix if file doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read cooccurrence file: {}", path.display()))?;
        let matrix: CooccurrenceMatrix = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse cooccurrence JSON: {}", path.display()))?;
        Ok(matrix)
    }

    /// Save to a JSON file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialize cooccurrence matrix")?;
        // Atomic write
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)
            .with_context(|| format!("Failed to write temp file: {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("Failed to rename temp to final: {}", path.display()))?;
        Ok(())
    }
}

/// Generate a workflow name from a cluster of pattern names.
/// Extracts common prefix segments from kebab-case names.
fn generate_cluster_name(names: &[String]) -> String {
    if names.is_empty() {
        return "unnamed-workflow".to_string();
    }

    // Try to find a common prefix from kebab-case segments
    let segments: Vec<Vec<&str>> = names
        .iter()
        .map(|n| n.split('-').collect::<Vec<_>>())
        .collect();

    let mut common_prefix = Vec::new();
    if let Some(first) = segments.first() {
        for (i, segment) in first.iter().enumerate() {
            if segments.iter().all(|s| s.get(i) == Some(segment)) {
                common_prefix.push(*segment);
            } else {
                break;
            }
        }
    }

    if common_prefix.is_empty() {
        // No common prefix — use first pattern's first segment + "workflow"
        let first_seg = segments
            .first()
            .and_then(|s| s.first())
            .unwrap_or(&"combined");
        format!("{}-workflow", first_seg)
    } else {
        format!("{}-workflow", common_prefix.join("-"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_cooccurrence_pair() {
        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&["a".into(), "b".into()]);
        assert_eq!(matrix.get_count("a", "b"), 1);
        assert_eq!(matrix.get_count("b", "a"), 1); // symmetric
    }

    #[test]
    fn test_record_cooccurrence_triple() {
        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&["a".into(), "b".into(), "c".into()]);
        assert_eq!(matrix.get_count("a", "b"), 1);
        assert_eq!(matrix.get_count("a", "c"), 1);
        assert_eq!(matrix.get_count("b", "c"), 1);
        assert_eq!(matrix.pair_count(), 3);
    }

    #[test]
    fn test_record_single_pattern_no_pairs() {
        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&["a".into()]);
        assert_eq!(matrix.pair_count(), 0);
    }

    #[test]
    fn test_record_empty_no_pairs() {
        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&[]);
        assert_eq!(matrix.pair_count(), 0);
    }

    #[test]
    fn test_accumulation() {
        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&["a".into(), "b".into()]);
        matrix.record_cooccurrence(&["a".into(), "b".into()]);
        matrix.record_cooccurrence(&["a".into(), "b".into()]);
        assert_eq!(matrix.get_count("a", "b"), 3);
    }

    #[test]
    fn test_canonical_pair_ordering() {
        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&["z-pattern".into(), "a-pattern".into()]);
        assert_eq!(matrix.get_count("a-pattern", "z-pattern"), 1);
        assert_eq!(matrix.get_count("z-pattern", "a-pattern"), 1);
    }

    #[test]
    fn test_save_load_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("cooccurrence.json");

        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&["rust-errors".into(), "rust-testing".into()]);
        matrix.record_cooccurrence(&["rust-errors".into(), "rust-testing".into()]);
        matrix.record_cooccurrence(&["swift-ui".into(), "swift-testing".into()]);

        matrix.save(&path).unwrap();
        let loaded = CooccurrenceMatrix::load(&path).unwrap();

        assert_eq!(loaded.get_count("rust-errors", "rust-testing"), 2);
        assert_eq!(loaded.get_count("swift-ui", "swift-testing"), 1);
        assert_eq!(loaded.pair_count(), 2);
    }

    #[test]
    fn test_load_nonexistent_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.json");
        let matrix = CooccurrenceMatrix::load(&path).unwrap();
        assert_eq!(matrix.pair_count(), 0);
    }

    #[test]
    fn test_find_clusters_basic() {
        let mut matrix = CooccurrenceMatrix::new();
        // Create a cluster of 3 patterns co-occurring 5+ times
        for _ in 0..6 {
            matrix.record_cooccurrence(&[
                "rust-errors".into(),
                "rust-testing".into(),
                "rust-logging".into(),
            ]);
        }
        // Add an unrelated pair below threshold
        matrix.record_cooccurrence(&["swift-ui".into(), "swift-nav".into()]);

        let clusters = matrix.find_clusters(5);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].pattern_names.len(), 3);
        assert!(clusters[0].pattern_names.contains(&"rust-errors".into()));
        assert!(clusters[0].pattern_names.contains(&"rust-testing".into()));
        assert!(clusters[0].pattern_names.contains(&"rust-logging".into()));
        assert!(clusters[0].total_cooccurrences >= 15); // 6 * 3 pairs = 18
    }

    #[test]
    fn test_find_clusters_no_results_below_threshold() {
        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&["a".into(), "b".into()]);
        let clusters = matrix.find_clusters(5);
        assert!(clusters.is_empty());
    }

    #[test]
    fn test_find_clusters_separate_components() {
        let mut matrix = CooccurrenceMatrix::new();
        // Cluster 1: rust patterns
        for _ in 0..5 {
            matrix.record_cooccurrence(&["rust-a".into(), "rust-b".into()]);
        }
        // Cluster 2: swift patterns (separate)
        for _ in 0..5 {
            matrix.record_cooccurrence(&["swift-a".into(), "swift-b".into()]);
        }
        let clusters = matrix.find_clusters(5);
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn test_generate_cluster_name_common_prefix() {
        let names = vec![
            "rust-errors".to_string(),
            "rust-testing".to_string(),
            "rust-logging".to_string(),
        ];
        let name = generate_cluster_name(&names);
        assert_eq!(name, "rust-workflow");
    }

    #[test]
    fn test_generate_cluster_name_no_common_prefix() {
        let names = vec!["alpha".to_string(), "beta".to_string()];
        let name = generate_cluster_name(&names);
        assert_eq!(name, "alpha-workflow");
    }

    #[test]
    fn test_all_pairs() {
        let mut matrix = CooccurrenceMatrix::new();
        matrix.record_cooccurrence(&["a".into(), "b".into(), "c".into()]);
        let pairs = matrix.all_pairs();
        assert_eq!(pairs.len(), 3);
    }
}
