//! Curator — applies Reflector deltas to the YAML pattern store.
//!
//! `curate` groups `ReflectResult`s by pattern name, detects conflicts
//! (both Reinforced and Contradicted in the same session), and writes
//! updated confidence + evidence signals back to the store.

use crate::capture::feedback::SignalType;
use crate::capture::reflector::ReflectResult;
use crate::store::yaml::YamlStore;

// ─── Public types ─────────────────────────────────────────────────────────────

/// A pattern that received conflicting signals in the same session.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub pattern_name: String,
    pub reason: String,
}

/// Summary returned by `curate`.
#[derive(Debug, Default)]
pub struct CuratorResult {
    /// Pattern names whose confidence was updated (or would be, in dry-run).
    pub updated: Vec<String>,
    /// Patterns that had conflicting signals; they are still updated.
    pub conflicts: Vec<Conflict>,
    /// Pattern names that were skipped (not in store, or all-Ignored).
    pub skipped: Vec<String>,
}

// ─── Entry point ─────────────────────────────────────────────────────────────

/// Apply Reflector deltas to the YAML store.
///
/// * Groups deltas by `pattern_name`.
/// * Skips patterns not present in the store.
/// * Detects conflicts (Reinforced + Contradicted in the same session).
/// * Applies net `confidence_delta`, clamped to `[0.0, 1.0]`.
/// * Updates `evidence.success_signals` / `evidence.override_signals`.
/// * When `dry_run` is `true`, computes everything but skips `store.save`.
pub fn curate(
    deltas: &[ReflectResult],
    store: &mut YamlStore,
    dry_run: bool,
) -> anyhow::Result<CuratorResult> {
    use std::collections::HashMap;

    // Group deltas by pattern name, preserving order of first occurrence.
    let mut groups: HashMap<String, Vec<&ReflectResult>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for delta in deltas {
        let entry = groups.entry(delta.pattern_name.clone()).or_insert_with(|| {
            order.push(delta.pattern_name.clone());
            Vec::new()
        });
        entry.push(delta);
    }

    let mut result = CuratorResult::default();

    for name in &order {
        let group = &groups[name];

        // Partition by signal type.
        let non_ignored: Vec<&&ReflectResult> = group
            .iter()
            .filter(|d| d.signal != SignalType::Ignored)
            .collect();

        if non_ignored.is_empty() {
            // All signals are Ignored — skip.
            if !result.skipped.contains(name) {
                result.skipped.push(name.clone());
            }
            continue;
        }

        // Check if pattern exists in the store.
        if !store.exists(name) {
            result.skipped.push(name.clone());
            continue;
        }

        // Conflict detection.
        let has_reinforced = non_ignored
            .iter()
            .any(|d| d.signal == SignalType::Reinforced);
        let has_contradicted = non_ignored
            .iter()
            .any(|d| d.signal == SignalType::Contradicted);
        if has_reinforced && has_contradicted {
            result.conflicts.push(Conflict {
                pattern_name: name.clone(),
                reason: "conflicting signals: reinforced and contradicted in same session"
                    .to_string(),
            });
        }

        // Net confidence delta (sum all non-ignored deltas).
        let net_delta: f32 = non_ignored.iter().map(|d| d.confidence_delta).sum();

        // Load, mutate, optionally save.
        let mut pattern = store.get(name)?;
        pattern.base.confidence = (pattern.base.confidence + net_delta as f64).clamp(0.0, 1.0);

        for delta in &non_ignored {
            match delta.signal {
                SignalType::Reinforced => pattern.base.evidence.success_signals += 1,
                SignalType::Contradicted => pattern.base.evidence.override_signals += 1,
                SignalType::Ignored => {}
            }
        }

        if !dry_run {
            store.save(&pattern)?;
        }

        result.updated.push(name.clone());
    }

    Ok(result)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::feedback::SignalType;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::Pattern;
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> YamlStore {
        YamlStore::new(dir.path().to_path_buf()).unwrap()
    }

    fn make_pattern(name: &str) -> Pattern {
        let mut kb = KnowledgeBase::default();
        kb.name = name.to_string();
        kb.description = "test".to_string();
        kb.confidence = 0.5;
        Pattern {
            base: kb,
            kind: None,
            origin: None,
            attachments: Vec::new(),
        }
    }

    fn make_delta(name: &str, signal: SignalType, delta: f32) -> ReflectResult {
        ReflectResult {
            pattern_name: name.to_string(),
            signal,
            evidence: None,
            confidence_delta: delta,
            llm_used: false,
        }
    }

    #[test]
    fn test_curate_reinforced_updates_confidence() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let p = make_pattern("test-pattern");
        store.save(&p).unwrap();
        let deltas = vec![make_delta("test-pattern", SignalType::Reinforced, 0.05)];
        let result = curate(&deltas, &mut store, false).unwrap();
        assert!(result.updated.contains(&"test-pattern".to_string()));
        let updated = store.get("test-pattern").unwrap();
        assert!(updated.confidence > 0.5);
    }

    #[test]
    fn test_curate_ignored_goes_to_skipped() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let p = make_pattern("test-pattern");
        store.save(&p).unwrap();
        let deltas = vec![make_delta("test-pattern", SignalType::Ignored, -0.01)];
        let result = curate(&deltas, &mut store, false).unwrap();
        assert!(result.skipped.contains(&"test-pattern".to_string()));
        assert!(result.updated.is_empty());
    }

    #[test]
    fn test_curate_conflict_detected() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let p = make_pattern("test-pattern");
        store.save(&p).unwrap();
        let deltas = vec![
            make_delta("test-pattern", SignalType::Reinforced, 0.05),
            make_delta("test-pattern", SignalType::Contradicted, -0.10),
        ];
        let result = curate(&deltas, &mut store, false).unwrap();
        assert!(!result.conflicts.is_empty());
        assert!(result.updated.contains(&"test-pattern".to_string()));
    }

    #[test]
    fn test_curate_dry_run_no_save() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let p = make_pattern("test-pattern");
        store.save(&p).unwrap();
        let deltas = vec![make_delta("test-pattern", SignalType::Reinforced, 0.05)];
        let result = curate(&deltas, &mut store, true).unwrap();
        assert!(!result.updated.is_empty());
        // Confidence should NOT change on disk in dry-run.
        let unchanged = store.get("test-pattern").unwrap();
        assert!((unchanged.confidence - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_curate_missing_pattern_skipped() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let deltas = vec![make_delta("nonexistent", SignalType::Reinforced, 0.05)];
        let result = curate(&deltas, &mut store, false).unwrap();
        assert!(result.skipped.contains(&"nonexistent".to_string()));
        assert!(result.updated.is_empty());
    }

    #[test]
    fn test_curate_clamps_confidence_to_one() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let mut p = make_pattern("test-pattern");
        p.base.confidence = 0.99;
        store.save(&p).unwrap();
        let deltas = vec![make_delta("test-pattern", SignalType::Reinforced, 0.2)];
        let result = curate(&deltas, &mut store, false).unwrap();
        assert!(!result.updated.is_empty());
        let updated = store.get("test-pattern").unwrap();
        assert!(updated.confidence <= 1.0);
    }

    #[test]
    fn test_curate_updates_success_signals() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let p = make_pattern("test-pattern");
        store.save(&p).unwrap();
        let deltas = vec![make_delta("test-pattern", SignalType::Reinforced, 0.05)];
        curate(&deltas, &mut store, false).unwrap();
        let updated = store.get("test-pattern").unwrap();
        assert_eq!(updated.base.evidence.success_signals, 1);
    }

    #[test]
    fn test_curate_updates_override_signals() {
        let dir = TempDir::new().unwrap();
        let mut store = make_store(&dir);
        let p = make_pattern("test-pattern");
        store.save(&p).unwrap();
        let deltas = vec![make_delta("test-pattern", SignalType::Contradicted, -0.10)];
        curate(&deltas, &mut store, false).unwrap();
        let updated = store.get("test-pattern").unwrap();
        assert_eq!(updated.base.evidence.override_signals, 1);
    }
}
