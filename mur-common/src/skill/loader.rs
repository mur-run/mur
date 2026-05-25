//! Single-pass skill loader: lists global + per-agent skills,
//! resolves trust level, checks drift, returns one flat Vec.

use crate::skill::types::TrustLevel;
use crate::skill::{DriftStatus, SkillManifest, content_sha256, drift_status, local};
use crate::trust::skills::SkillTrustStore;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillScope {
    Global,
    Agent,
}

#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub name: String,
    pub manifest: SkillManifest,
    pub trust: TrustLevel,
    pub scope: SkillScope,
    pub content_hash: String,
}

pub fn load_all(mur_home: &Path, agent_name: &str) -> Vec<LoadedSkill> {
    let trust = SkillTrustStore::load(mur_home).unwrap_or_default();
    let mut out: Vec<LoadedSkill> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = Default::default();

    // Per-agent first (wins on name collision)
    if let Ok(names) = local::list_installed_agent(mur_home, agent_name) {
        for name in names {
            if let Some(loaded) = load_one(mur_home, &name, SkillScope::Agent, &trust, |m, n| {
                local::load_installed_agent(m, agent_name, n)
            }) {
                seen_names.insert(loaded.name.clone());
                out.push(loaded);
            }
        }
    }
    if let Ok(names) = local::list_installed(mur_home) {
        for name in names {
            if seen_names.contains(&name) {
                continue;
            }
            if let Some(loaded) = load_one(
                mur_home,
                &name,
                SkillScope::Global,
                &trust,
                local::load_installed,
            ) {
                out.push(loaded);
            }
        }
    }
    out
}

fn load_one<F>(
    mur_home: &Path,
    name: &str,
    scope: SkillScope,
    trust: &SkillTrustStore,
    loader: F,
) -> Option<LoadedSkill>
where
    F: FnOnce(&Path, &str) -> Result<SkillManifest, crate::skill::StoreError>,
{
    let manifest = match loader(mur_home, name) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(skill = %name, error = %e, "skill load failed; skipping");
            return None;
        }
    };
    let hash = match content_sha256(&manifest) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(skill = %name, error = %e, "skill hash failed; skipping");
            return None;
        }
    };
    // Drift check: if there's a pinned hash for this skill in the trust store
    // and it disagrees, refuse to load.
    let entry = trust.entries.get(&hash);
    if let Some(pinned) = entry {
        if let Ok(DriftStatus::Drift { expected, actual }) = drift_status(&manifest, Some(&hash)) {
            tracing::warn!(skill = %name, expected, actual, "skill drift detected; skipping");
            return None;
        }
        if trust.is_revoked(&hash) {
            tracing::warn!(skill = %name, "skill hash revoked; skipping");
            return None;
        }
        Some(LoadedSkill {
            name: name.into(),
            manifest,
            trust: pinned.level,
            scope,
            content_hash: hash,
        })
    } else {
        // Unpinned = first-load Sandboxed.
        Some(LoadedSkill {
            name: name.into(),
            manifest,
            trust: TrustLevel::Sandboxed,
            scope,
            content_hash: hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{parse_canonical, write_to_dir};
    use tempfile::tempdir;

    fn make(name: &str) -> SkillManifest {
        parse_canonical(&format!(
            r#"name: {name}
version: 1.0.0
publisher: human:t
description: test
category: context
content:
  abstract: hi
  context: body
"#
        ))
        .unwrap()
    }

    #[test]
    fn empty_mur_home_returns_empty() {
        let dir = tempdir().unwrap();
        let loaded = load_all(dir.path(), "alice");
        assert!(loaded.is_empty());
    }

    #[test]
    fn global_skill_returns_sandboxed_when_no_trust_entry() {
        let dir = tempdir().unwrap();
        write_to_dir(&dir.path().join("skills").join("demo"), &make("demo")).unwrap();
        let loaded = load_all(dir.path(), "alice");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "demo");
        assert_eq!(loaded[0].trust, TrustLevel::Sandboxed);
        assert_eq!(loaded[0].scope, SkillScope::Global);
    }

    #[test]
    fn agent_overrides_global_by_name() {
        let dir = tempdir().unwrap();
        // Both global and agent have "shared"
        write_to_dir(&dir.path().join("skills").join("shared"), &make("shared")).unwrap();
        write_to_dir(
            &dir.path()
                .join("agents")
                .join("alice")
                .join("skills")
                .join("shared"),
            &make("shared"),
        )
        .unwrap();
        let loaded = load_all(dir.path(), "alice");
        let shared: Vec<_> = loaded.iter().filter(|s| s.name == "shared").collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].scope, SkillScope::Agent);
    }
}
