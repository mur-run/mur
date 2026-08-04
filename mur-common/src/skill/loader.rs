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

/// Outcome of resolving a `profile.yaml` skill ref (e.g. `skills/<name>`)
/// against an agent's home directory.
///
/// Distinguishing `Missing` from `Malformed` matters: a ref written without
/// installing the backing files (issue #717) is a *missing* skill — telling
/// the user it "no longer parses" points them at the wrong root cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillRefStatus {
    /// The ref resolves to a manifest that parses and validates.
    Loadable,
    /// No file exists at the resolved manifest path.
    Missing { path: std::path::PathBuf },
    /// A file exists but does not parse/validate as a skill manifest.
    Malformed {
        path: std::path::PathBuf,
        error: String,
    },
}

/// Resolve a `profile.yaml` skill ref to its backing manifest file and report
/// whether it is loadable, missing, or malformed.
///
/// Resolution mirrors the runtime loader's layout rules: modern refs point at
/// a *directory* (`skills/<name>`) holding `skill.yaml`; legacy refs may point
/// directly at a `.yaml`/`.yml`/`.md` file. This is the single source of truth
/// for ref resolution — the Hub loadability badge and the creation-time
/// validation in mur-core both call it.
pub fn skill_ref_status(agent_home: &Path, rel_ref: &str) -> SkillRefStatus {
    let joined = agent_home.join(rel_ref);
    let ext = joined
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let file = if joined.is_dir() || !matches!(ext.as_str(), "yaml" | "yml" | "md" | "markdown") {
        // Modern directory layout: the ref names the skill dir; the manifest
        // lives inside it. Also used when the dir is absent, so the Missing
        // path names the exact manifest we expected to find.
        joined.join("skill.yaml")
    } else {
        joined
    };
    if !file.is_file() {
        return SkillRefStatus::Missing { path: file };
    }
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            return SkillRefStatus::Malformed {
                path: file,
                error: format!("unreadable: {e}"),
            };
        }
    };
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let parsed = match ext.as_str() {
        "yaml" | "yml" => crate::skill::parse_canonical(&text),
        "md" | "markdown" => crate::skill::parse_markdown(&text)
            .or_else(|_| crate::skill::parse_legacy_markdown(&text)),
        other => {
            return SkillRefStatus::Malformed {
                path: file,
                error: format!("unsupported manifest extension '.{other}'"),
            };
        }
    };
    match parsed {
        Ok(m) => match crate::skill::validate(&m) {
            Ok(()) => SkillRefStatus::Loadable,
            Err(e) => SkillRefStatus::Malformed {
                path: file,
                error: format!("invalid manifest: {e}"),
            },
        },
        Err(e) => SkillRefStatus::Malformed {
            path: file,
            error: format!("parse failed: {e}"),
        },
    }
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
    // Federated knowledge cache next (daemon-assembled snapshot of global
    // skills at or above the lifecycle floor; federation P0). A cache entry
    // wins over a same-named global — it IS that global skill, scope-filtered
    // — but never over a per-agent install.
    let cache_dir = mur_home
        .join("agents")
        .join(agent_name)
        .join("knowledge_cache");
    if let Ok(entries) = std::fs::read_dir(&cache_dir) {
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("skill.yaml").is_file())
            .filter_map(|e| e.file_name().to_str().map(String::from))
            .collect();
        names.sort(); // deterministic load order
        for name in names {
            if seen_names.contains(&name) {
                continue;
            }
            let dir = cache_dir.join(&name);
            let dir_for_loader = dir.clone();
            if let Some(mut loaded) =
                load_one(mur_home, &name, SkillScope::Global, &trust, move |_m, _n| {
                    crate::skill::read_from_dir(&dir_for_loader)
                })
            {
                loaded.dir = dir;
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
        make_desc(name, "test")
    }

    fn make_desc(name: &str, desc: &str) -> SkillManifest {
        parse_canonical(&format!(
            r#"name: {name}
version: 1.0.0
publisher: human:t
description: {desc}
category: context
content:
  abstract: hi
  context: body
"#
        ))
        .unwrap()
    }

    #[test]
    fn knowledge_cache_skill_loads() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let cdir = home
            .join("agents/a1/knowledge_cache")
            .join("federated-skill");
        write_to_dir(&cdir, &make("federated-skill")).unwrap();

        let loaded = load_all(home, "a1");
        let hit = loaded
            .iter()
            .find(|s| s.name == "federated-skill")
            .expect("cached skill must be visible to the loader");
        assert_eq!(hit.dir, cdir);
    }

    #[test]
    fn agent_local_wins_over_cache_wins_over_global() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        write_to_dir(
            &home.join("agents/a1/skills/dup"),
            &make_desc("dup", "agent-local"),
        )
        .unwrap();
        write_to_dir(
            &home.join("agents/a1/knowledge_cache/dup"),
            &make_desc("dup", "cache"),
        )
        .unwrap();
        write_to_dir(&home.join("skills/dup"), &make_desc("dup", "global")).unwrap();

        let loaded = load_all(home, "a1");
        let dups: Vec<_> = loaded.iter().filter(|s| s.name == "dup").collect();
        assert_eq!(dups.len(), 1, "name collision must resolve to ONE copy");
        assert_eq!(dups[0].manifest.description, "agent-local");

        // Remove the per-agent copy: the cache copy takes over, not the global.
        std::fs::remove_dir_all(home.join("agents/a1/skills/dup")).unwrap();
        let loaded = load_all(home, "a1");
        let dup = loaded.iter().find(|s| s.name == "dup").unwrap();
        assert_eq!(dup.manifest.description, "cache");
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

    // ── skill_ref_status (#717): missing vs malformed ────────────────────

    #[test]
    fn skill_ref_status_loadable_for_installed_dir_skill() {
        let home = tempdir().unwrap();
        write_to_dir(&home.path().join("skills").join("demo"), &make("demo")).unwrap();
        assert_eq!(
            skill_ref_status(home.path(), "skills/demo"),
            SkillRefStatus::Loadable
        );
    }

    #[test]
    fn skill_ref_status_absent_ref_is_missing_with_manifest_path() {
        let home = tempdir().unwrap();
        match skill_ref_status(home.path(), "skills/executing-plans") {
            SkillRefStatus::Missing { path } => {
                // The reported path names the exact manifest we expected.
                assert!(path.ends_with("skills/executing-plans/skill.yaml"));
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn skill_ref_status_garbage_yaml_is_malformed() {
        let home = tempdir().unwrap();
        let sdir = home.path().join("skills").join("broken");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(sdir.join("skill.yaml"), "{{{ not: [valid").unwrap();
        assert!(matches!(
            skill_ref_status(home.path(), "skills/broken"),
            SkillRefStatus::Malformed { .. }
        ));
    }

    #[test]
    fn skill_ref_status_legacy_md_file_resolves_directly() {
        let home = tempdir().unwrap();
        let sdir = home.path().join("skills");
        std::fs::create_dir_all(&sdir).unwrap();
        // Absent legacy .md ref → Missing at the file itself (no /skill.yaml).
        match skill_ref_status(home.path(), "skills/old.md") {
            SkillRefStatus::Missing { path } => assert!(path.ends_with("skills/old.md")),
            other => panic!("expected Missing, got {other:?}"),
        }
        // Present but unparseable legacy .md → Malformed.
        std::fs::write(sdir.join("old.md"), "no frontmatter here").unwrap();
        assert!(matches!(
            skill_ref_status(home.path(), "skills/old.md"),
            SkillRefStatus::Malformed { .. }
        ));
    }
}
