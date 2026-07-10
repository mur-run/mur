//! Single-pass skill loader: lists global + per-agent skills,
//! resolves trust level, checks drift, returns one flat Vec.

use crate::skill::types::TrustLevel;
use crate::skill::{DriftStatus, SkillManifest, content_sha256, drift_status, local};
use crate::trust::skills::SkillTrustStore;
use std::path::Path;

/// Validate that a skill name contains only safe identifier characters.
///
/// Skill names are interpolated into XML-like `<skill-instruction source="…">`
/// attributes.  Restricting the character set at load time means injection is
/// blocked at the source rather than relying solely on escaping at emit time.
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        // Reserved path components: a skill name is joined into
        // `<mur_home>/skills/<name>`, so `.`/`..` must never be accepted.
        && name != "."
        && name != ".."
        // The character set already excludes `/` and `\`, which keeps a name to
        // a single path component (no traversal into sibling/parent dirs).
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

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
    /// Absolute install directory of this skill (holds skill.yaml + any bundle).
    pub dir: std::path::PathBuf,
}

pub fn load_all(mur_home: &Path, agent_name: &str) -> Vec<LoadedSkill> {
    let trust = SkillTrustStore::load(mur_home).unwrap_or_default();
    let mut out: Vec<LoadedSkill> = Vec::new();
    let mut seen_names: std::collections::HashSet<String> = Default::default();

    // Per-agent first (wins on name collision)
    if let Ok(names) = local::list_installed_agent(mur_home, agent_name) {
        for name in names {
            // Skip non-skill dirs (e.g. a fleet run-ledger `fleet:<name>/`
            // written under skills/ by the DAG executor's record_run — it holds
            // events.jsonl, not skill.yaml). Without this, its colon name trips
            // is_valid_skill_name in load_one and spams a warning every load.
            if !crate::skill::store::agent_skill_dir(mur_home, agent_name)
                .join(&name)
                .join("skill.yaml")
                .is_file()
            {
                continue;
            }
            if let Some(mut loaded) =
                load_one(mur_home, &name, SkillScope::Agent, &trust, |m, n| {
                    local::load_installed_agent(m, agent_name, n)
                })
            {
                loaded.dir = crate::skill::store::agent_skill_dir(mur_home, agent_name).join(&name);
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
            // Skip non-skill dirs (see the agent loop above) — a manifest-less
            // dir is a ledger/data dir, not a skill.
            if !crate::skill::store::global_skill_dir(mur_home, &name)
                .join("skill.yaml")
                .is_file()
            {
                continue;
            }
            if let Some(mut loaded) = load_one(
                mur_home,
                &name,
                SkillScope::Global,
                &trust,
                local::load_installed,
            ) {
                loaded.dir = crate::skill::store::global_skill_dir(mur_home, &name);
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
    // Validate name before loading: only safe identifier characters allowed.
    // Skill names are interpolated into XML attributes; an unvalidated name
    // containing `"` or `>` could break the attribute boundary even after
    // escaping if the validator itself is bypassed.
    if !is_valid_skill_name(name) {
        tracing::warn!(
            skill = %name,
            "skill name contains invalid characters (expected [A-Za-z0-9_.-]{{1,64}}); skipping"
        );
        return None;
    }

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
            dir: std::path::PathBuf::new(), // overwritten by load_all
        })
    } else {
        // Unpinned = first-load Sandboxed.
        Some(LoadedSkill {
            name: name.into(),
            manifest,
            trust: TrustLevel::Sandboxed,
            scope,
            content_hash: hash,
            dir: std::path::PathBuf::new(), // overwritten by load_all
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{parse_canonical, write_to_dir};
    use tempfile::tempdir;

    #[test]
    fn load_all_sets_agent_skill_dir() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let sdir = home.join("agents").join("a1").join("skills").join("demo");
        write_to_dir(&sdir, &make("demo")).unwrap();

        let loaded = load_all(home, "a1");
        let demo = loaded.iter().find(|s| s.name == "demo").unwrap();
        assert_eq!(demo.dir, sdir);
    }

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
    fn load_all_skips_non_skill_dirs() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        // A real global skill (has skill.yaml)…
        write_to_dir(&home.join("skills").join("real"), &make("real")).unwrap();
        // …and a non-skill dir under skills/ (only events.jsonl, no skill.yaml) —
        // e.g. a fleet run-ledger. Uses a portable name here: the real ledger id
        // is `fleet:<name>`, but a colon is an illegal filename on Windows, so
        // the test fixture would fail to even create it. The skip logic keys on
        // the absent skill.yaml, not the name.
        let ledger = home.join("skills").join("not-a-skill");
        std::fs::create_dir_all(&ledger).unwrap();
        std::fs::write(ledger.join("events.jsonl"), "{}\n").unwrap();

        let loaded = load_all(home, "a1");
        let names: Vec<_> = loaded.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["real"],
            "ledger dir must not be loaded as a skill"
        );
    }

    #[test]
    fn is_valid_skill_name_rejects_traversal_and_reserved() {
        // Legit names.
        assert!(is_valid_skill_name("web-search"));
        assert!(is_valid_skill_name("my.skill_v2"));
        // Reserved path components.
        assert!(!is_valid_skill_name("."));
        assert!(!is_valid_skill_name(".."));
        // Path separators (the dangerous traversal form) and absolutes.
        assert!(!is_valid_skill_name("../agents/victim/skills/evil"));
        assert!(!is_valid_skill_name("a/b"));
        assert!(!is_valid_skill_name("a\\b"));
        assert!(!is_valid_skill_name("/etc/passwd"));
        // Bounds.
        assert!(!is_valid_skill_name(""));
        assert!(!is_valid_skill_name(&"x".repeat(65)));
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
