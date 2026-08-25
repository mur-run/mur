//! Recursive resolver for `mur skill install` — DFS with cycle detection,
//! semver constraint matching, leaves-first install ordering.

use anyhow::Result;
use mur_common::skill::{
    Constraint, ConstraintError, SkillManifest, parse_canonical, parse_markdown, validate,
};
use semver::Version;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("cyclic dependency: {0}")]
    Cycle(String),
    #[error(
        "no version of '{name}' satisfies '{req}' (existing pin: {existing}); available: {available:?}"
    )]
    Conflict {
        name: String,
        req: String,
        existing: String,
        available: Vec<String>,
    },
    #[error("no version of '{name}' satisfies '{req}'; available: {available:?}")]
    NoMatch {
        name: String,
        req: String,
        available: Vec<String>,
    },
    #[error("skill '{0}' not found in registry cache")]
    NotFound(String),
    #[error("constraint parse: {0}")]
    BadConstraint(#[from] ConstraintError),
    #[error("manifest parse: {0}")]
    BadManifest(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct ResolvedNode {
    pub name: String,
    pub version: Version,
    pub yaml_path: PathBuf,
    pub manifest: SkillManifest,
}

pub enum ResolveSource<'a> {
    LocalFile(&'a Path),
    RegistryLatest(&'a str),
}

pub struct ResolverInput {
    pub mur_home: PathBuf,
    pub registry_dir: PathBuf,
}

pub fn resolve(
    input: &ResolverInput,
    source: ResolveSource<'_>,
) -> Result<Vec<ResolvedNode>, ResolveError> {
    let mut state = State::default();
    let root = load_root(input, source)?;
    visit(input, &mut state, root, Constraint::any())?;
    Ok(state
        .install_order
        .into_iter()
        .map(|n| state.selected.remove(&n).unwrap())
        .collect())
}

#[derive(Default)]
struct State {
    selected: BTreeMap<String, ResolvedNode>,
    on_stack: Vec<String>,
    install_order: Vec<String>,
}

fn load_root(input: &ResolverInput, src: ResolveSource<'_>) -> Result<ResolvedNode, ResolveError> {
    match src {
        ResolveSource::LocalFile(p) => load_from_path(p),
        ResolveSource::RegistryLatest(name) => {
            let versions =
                crate::cmd::skill_registry::available_versions(&input.registry_dir, name)
                    .map_err(ResolveError::Other)?;
            let v = versions
                .last()
                .cloned()
                .ok_or_else(|| ResolveError::NotFound(name.into()))?;
            let path = crate::cmd::skill_registry::skill_yaml_path(
                &input.registry_dir,
                name,
                &v.to_string(),
            );
            load_from_path(&path)
        }
    }
}

fn load_from_path(path: &Path) -> Result<ResolvedNode, ResolveError> {
    let text = std::fs::read_to_string(path).map_err(|e| ResolveError::Other(e.into()))?;
    // Pick the parser by extension, matching `mur agent skill add`: `.yaml`/
    // `.yml` are canonical manifests, anything else is markdown-with-frontmatter.
    // Without this, `mur skill install foo.md` failed with the canonical
    // parser's "missing field `content`" — a message about the YAML schema for
    // a file that was never YAML — while the per-agent install of the same file
    // succeeded.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let parsed = if ext == "yaml" || ext == "yml" {
        parse_canonical(&text)
    } else {
        parse_markdown(&text)
    };
    let m = parsed.map_err(|e| ResolveError::BadManifest(e.to_string()))?;
    validate(&m).map_err(|e| ResolveError::BadManifest(e.to_string()))?;
    let v = Version::parse(&m.version)
        .map_err(|e| ResolveError::BadManifest(format!("version: {e}")))?;
    Ok(ResolvedNode {
        name: m.name.clone(),
        version: v,
        yaml_path: path.to_path_buf(),
        manifest: m,
    })
}

fn pick_best(
    input: &ResolverInput,
    name: &str,
    c: &Constraint,
) -> Result<ResolvedNode, ResolveError> {
    let versions = crate::cmd::skill_registry::available_versions(&input.registry_dir, name)
        .map_err(ResolveError::Other)?;
    let mut candidates: Vec<&Version> = versions.iter().filter(|v| c.matches(v)).collect();
    candidates.sort();
    let pick = candidates.last().ok_or_else(|| ResolveError::NoMatch {
        name: name.into(),
        req: c.0.to_string(),
        available: versions.iter().map(|v| v.to_string()).collect(),
    })?;
    let path =
        crate::cmd::skill_registry::skill_yaml_path(&input.registry_dir, name, &pick.to_string());
    load_from_path(&path)
}

fn visit(
    input: &ResolverInput,
    state: &mut State,
    node: ResolvedNode,
    requested: Constraint,
) -> Result<(), ResolveError> {
    if state.on_stack.iter().any(|n| n == &node.name) {
        let mut path = state.on_stack.clone();
        path.push(node.name.clone());
        return Err(ResolveError::Cycle(path.join(" -> ")));
    }
    if let Some(existing) = state.selected.get(&node.name) {
        if !requested.matches(&existing.version) {
            return Err(ResolveError::Conflict {
                name: node.name,
                req: requested.0.to_string(),
                existing: existing.version.to_string(),
                available: vec![existing.version.to_string()],
            });
        }
        return Ok(());
    }
    state.on_stack.push(node.name.clone());
    let name = node.name.clone();
    let requires = node.manifest.requires.clone();
    state.selected.insert(name.clone(), node);

    for req in requires {
        let c = Constraint::parse(&req.version)?;
        let chosen = pick_best(input, &req.name, &c)?;
        visit(input, state, chosen, c)?;
    }

    state.install_order.push(name);
    state.on_stack.pop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// A `.md` skill file must resolve, not die on the canonical parser.
    /// `mur agent skill add` has always accepted markdown; `mur skill install`
    /// rejected the same file with "missing field `content`" — a YAML-schema
    /// error about a file that was never YAML.
    #[test]
    fn local_markdown_source_resolves() {
        let tmp = tempdir().unwrap();
        let md = tmp.path().join("my-file.md");
        fs::write(
            &md,
            "---\nname: from-markdown\nversion: 1.0.0\npublisher: human:test\n\
             description: markdown source\ncategory: context\n---\n\n\
             # from-markdown\n\nbody text\n",
        )
        .unwrap();

        let node = load_from_path(&md).expect("markdown source should resolve");
        // Named from the manifest, not the filename ("my-file").
        assert_eq!(node.name, "from-markdown");
        assert_eq!(node.version.to_string(), "1.0.0");
    }

    /// Negative control: `.yaml` still goes through the canonical parser, so a
    /// real YAML schema error is still reported as one.
    #[test]
    fn local_yaml_source_still_uses_canonical_parser() {
        let tmp = tempdir().unwrap();
        let y = tmp.path().join("broken.yaml");
        fs::write(&y, "name: broken\nversion: 1.0.0\n").unwrap();
        let err = load_from_path(&y).expect_err("incomplete yaml must fail");
        assert!(
            matches!(err, ResolveError::BadManifest(_)),
            "expected BadManifest, got: {err}"
        );
    }

    fn write_skill(reg_dir: &Path, name: &str, version: &str, requires: &[(&str, &str)]) {
        let vdir = reg_dir.join("skills").join(name).join("versions");
        fs::create_dir_all(&vdir).unwrap();
        let mut yaml = format!(
            "name: {name}\nversion: {version}\npublisher: human:test\ndescription: test\ncategory: context\ncontent:\n  abstract: a\n  context: b\n"
        );
        if !requires.is_empty() {
            yaml.push_str("requires:\n");
            for (n, v) in requires {
                yaml.push_str(&format!("  - name: {n}\n    version: \"{v}\"\n"));
            }
        }
        fs::write(vdir.join(format!("{version}.yaml")), yaml).unwrap();
    }

    fn make_input(reg_dir: &Path) -> ResolverInput {
        ResolverInput {
            mur_home: reg_dir.to_path_buf(),
            registry_dir: reg_dir.to_path_buf(),
        }
    }

    #[test]
    fn single_skill_no_requires() {
        let d = tempdir().unwrap();
        write_skill(d.path(), "solo", "1.0.0", &[]);
        let input = make_input(d.path());
        let nodes = resolve(&input, ResolveSource::RegistryLatest("solo")).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "solo");
        assert_eq!(nodes[0].version.to_string(), "1.0.0");
    }

    #[test]
    fn transitive_picks_highest_and_leaves_first_order() {
        let d = tempdir().unwrap();
        write_skill(d.path(), "dep-b", "1.0.0", &[]);
        write_skill(d.path(), "dep-b", "1.1.0", &[]);
        write_skill(d.path(), "root", "0.1.0", &[("dep-b", ">=1.0.0")]);
        let input = make_input(d.path());
        let nodes = resolve(&input, ResolveSource::RegistryLatest("root")).unwrap();
        assert_eq!(nodes.len(), 2, "root + dep-b");
        // Leaves first: dep-b before root
        assert_eq!(nodes[0].name, "dep-b");
        assert_eq!(nodes[0].version.to_string(), "1.1.0");
        assert_eq!(nodes[1].name, "root");
    }

    #[test]
    fn conflict_when_constraints_clash() {
        let d = tempdir().unwrap();
        // A requires B 1.x, C requires B 2.x; A also requires C
        write_skill(d.path(), "dep-b", "1.0.0", &[]);
        write_skill(d.path(), "dep-b", "2.0.0", &[]);
        write_skill(d.path(), "dep-c", "1.0.0", &[("dep-b", "^2.0.0")]);
        write_skill(
            d.path(),
            "root",
            "0.1.0",
            &[("dep-b", "^1.0.0"), ("dep-c", "*")],
        );
        let input = make_input(d.path());
        let err = resolve(&input, ResolveSource::RegistryLatest("root")).unwrap_err();
        assert!(
            matches!(err, ResolveError::Conflict { .. }),
            "expected Conflict, got: {err}"
        );
    }

    #[test]
    fn cycle_detection_direct() {
        let d = tempdir().unwrap();
        write_skill(d.path(), "alpha", "1.0.0", &[("beta", "*")]);
        write_skill(d.path(), "beta", "1.0.0", &[("alpha", "*")]);
        let input = make_input(d.path());
        let err = resolve(&input, ResolveSource::RegistryLatest("alpha")).unwrap_err();
        assert!(
            matches!(err, ResolveError::Cycle(_)),
            "expected Cycle, got: {err}"
        );
    }

    #[test]
    fn no_match_when_versions_too_low() {
        let d = tempdir().unwrap();
        write_skill(d.path(), "dep-b", "1.0.0", &[]);
        write_skill(d.path(), "dep-b", "1.1.0", &[]);
        write_skill(d.path(), "root", "0.1.0", &[("dep-b", "^2.0.0")]);
        let input = make_input(d.path());
        let err = resolve(&input, ResolveSource::RegistryLatest("root")).unwrap_err();
        assert!(
            matches!(err, ResolveError::NoMatch { .. }),
            "expected NoMatch, got: {err}"
        );
    }

    #[test]
    fn diamond_dep_appears_once() {
        let d = tempdir().unwrap();
        write_skill(d.path(), "dep-d", "1.0.0", &[]);
        write_skill(d.path(), "dep-b", "1.0.0", &[("dep-d", "1.x")]);
        write_skill(d.path(), "dep-c", "1.0.0", &[("dep-d", "1.x")]);
        write_skill(d.path(), "root", "0.1.0", &[("dep-b", "*"), ("dep-c", "*")]);
        let input = make_input(d.path());
        let nodes = resolve(&input, ResolveSource::RegistryLatest("root")).unwrap();
        // dep-d should appear exactly once
        let d_count = nodes.iter().filter(|n| n.name == "dep-d").count();
        assert_eq!(
            d_count, 1,
            "diamond dep appeared {d_count} times, expected 1"
        );
        // Total: root + b + c + d = 4
        assert_eq!(nodes.len(), 4);
    }

    #[test]
    fn garbage_version_in_requires_is_bad_constraint() {
        let d = tempdir().unwrap();
        write_skill(d.path(), "dep-b", "1.0.0", &[]);
        write_skill(d.path(), "root", "0.1.0", &[("dep-b", "not-semver")]);
        let input = make_input(d.path());
        let err = resolve(&input, ResolveSource::RegistryLatest("root")).unwrap_err();
        assert!(
            matches!(err, ResolveError::BadConstraint(_)),
            "expected BadConstraint, got: {err}"
        );
    }
}
