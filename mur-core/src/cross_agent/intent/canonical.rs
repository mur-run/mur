//! Host-level intent canonicaliser (M7c §3.6).
//!
//! Scans every installed manifest under `<home>/agents/<a>/skills/<s>/skill.yaml`,
//! collects `ProcedureStep::intent` strings, clusters by normalised form,
//! and writes the most-frequent original spelling per cluster as the
//! canonical for that cluster. File: `<home>/intent_canonical.yaml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::peers::list_peer_agents;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalEntry {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentCanonical {
    pub version: u32,
    pub generated_at: DateTime<Utc>,
    pub generated_by: String,
    pub canonical: Vec<CanonicalEntry>,
}

pub fn canonical_path(home: &Path) -> PathBuf {
    home.join("intent_canonical.yaml")
}

pub fn build_canonical(home: &Path, generated_by: &str) -> Result<IntentCanonical> {
    // (normalised_form, original_string) -> count
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();

    let agents = list_peer_agents(home)?;
    for agent in &agents {
        let skills_dir = home.join("agents").join(&agent.name).join("skills");
        if !skills_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&skills_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest_path = entry.path().join("skill.yaml");
            if !manifest_path.exists() {
                continue;
            }
            let bytes = match std::fs::read(&manifest_path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let manifest: SkillManifest = match serde_yaml_ng::from_slice(&bytes) {
                Ok(m) => m,
                Err(_) => continue,
            };
            collect_intents(&manifest, &mut counts);
        }
    }

    let mut canonical_entries: Vec<CanonicalEntry> = counts
        .into_iter()
        .map(|(_norm, originals)| {
            let total: usize = originals.values().sum();
            let mut sorted: Vec<(String, usize)> = originals.into_iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let canonical = sorted[0].0.clone();
            let aliases: Vec<String> = sorted.into_iter().map(|(s, _)| s).collect();
            CanonicalEntry {
                canonical,
                aliases,
                count: total,
            }
        })
        .collect();
    canonical_entries.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.canonical.cmp(&b.canonical))
    });

    Ok(IntentCanonical {
        version: 1,
        generated_at: Utc::now(),
        generated_by: generated_by.to_string(),
        canonical: canonical_entries,
    })
}

fn collect_intents(
    manifest: &SkillManifest,
    counts: &mut BTreeMap<String, BTreeMap<String, usize>>,
) {
    if let Some(proc) = &manifest.content.procedure {
        for step in &proc.steps {
            if let Some(intent) = &step.intent {
                let norm = normalise(intent);
                counts
                    .entry(norm)
                    .or_default()
                    .entry(intent.to_string())
                    .and_modify(|c| *c += 1)
                    .or_insert(1);
            }
        }
    }
}

/// Lowercase, collapse runs of whitespace/hyphens/underscores to `_`,
/// strip leading/trailing `_`.
pub fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_sep = true;
    for ch in s.chars() {
        if ch.is_whitespace() || ch == '_' || ch == '-' {
            if !last_was_sep {
                out.push('_');
                last_was_sep = true;
            }
        } else {
            out.extend(ch.to_lowercase());
            last_was_sep = false;
        }
    }
    out.trim_matches('_').to_string()
}

pub fn write_canonical_yaml(home: &Path, ic: &IntentCanonical) -> Result<()> {
    let path = canonical_path(home);
    let yaml = serde_yaml_ng::to_string(ic).context("serialise IntentCanonical")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

pub fn read_canonical_yaml(home: &Path) -> Result<Option<IntentCanonical>> {
    let path = canonical_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let ic: IntentCanonical = serde_yaml_ng::from_slice(&bytes)?;
    Ok(Some(ic))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_handles_separators() {
        assert_eq!(normalise("Web Search"), "web_search");
        assert_eq!(normalise("web-search"), "web_search");
        assert_eq!(normalise("WEB__SEARCH"), "web_search");
        assert_eq!(normalise("  web   search  "), "web_search");
    }

    #[test]
    fn empty_input_yields_empty_norm() {
        assert_eq!(normalise(""), "");
        assert_eq!(normalise("   "), "");
    }

    #[test]
    fn canonical_picks_most_frequent_then_alphabetical() {
        let mut originals: BTreeMap<String, usize> = BTreeMap::new();
        originals.insert("Web Search".into(), 3);
        originals.insert("web_search".into(), 3);
        originals.insert("web search".into(), 1);
        let mut sorted: Vec<(String, usize)> = originals.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        assert_eq!(sorted[0].0, "Web Search");
    }
}
