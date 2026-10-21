//! Which languages a project uses, from the marker files in
//! [`Lang::markers`] (#2016). Local and read-only: a shallow, bounded walk
//! that never follows a symlink, skips the indexer's vendored/build dirs and
//! what the root `.gitignore` names, and runs nothing.

use std::collections::BTreeSet;
use std::path::Path;

use super::super::plan::Lang;
use crate::codebase::scanner::{SKIP_DIRS, is_gitignored, load_gitignore};

/// How deep below the project root markers are looked for. Deep enough for
/// a `frontend/package.json` or `src/app/App.vue`, shallow enough to stay
/// cheap on a monorepo.
pub(super) const MAX_DEPTH: usize = 3;
/// Directory entries examined per project before the walk stops.
pub(super) const MAX_ENTRIES: usize = 20_000;

/// Languages whose markers appear in `root`, in [`Lang::ALL`] order.
pub(super) fn detect_langs(root: &Path) -> BTreeSet<Lang> {
    let ignore = load_gitignore(root);
    let mut found = BTreeSet::new();
    let mut budget = MAX_ENTRIES;
    walk(root, root, 0, &ignore, &mut budget, &mut found);
    found
}

fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    ignore: &[String],
    budget: &mut usize,
    found: &mut BTreeSet<Lang>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    for entry in rd.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // `file_type` does not follow symlinks: a link is neither.
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if is_gitignored(&rel, ignore, ft.is_dir()) {
            continue;
        }
        for lang in Lang::ALL {
            if lang.markers().iter().any(|m| m.matches(&name)) {
                found.insert(lang);
            }
        }
        if ft.is_dir() && !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
            subdirs.push(entry.path());
        }
    }
    if depth + 1 >= MAX_DEPTH {
        return;
    }
    subdirs.sort();
    for d in subdirs {
        walk(root, &d, depth + 1, ignore, budget, found);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"").unwrap();
    }

    #[test]
    fn markers_map_to_languages() {
        let t = tempfile::tempdir().unwrap();
        for f in [
            "Cargo.toml",
            "web/package.json",
            "web/src/App.vue",
            "requirements-dev.txt",
            "composer.json",
            "go.mod",
            "pom.xml",
            "app/Main.kt",
            "Package.swift",
            "Gemfile",
            "CMakeLists.txt",
            "scripts/init.lua",
        ] {
            touch(t.path(), f);
        }
        assert_eq!(detect_langs(t.path()), Lang::ALL.into_iter().collect());
    }

    #[test]
    fn xcodeproj_dir_counts_as_swift() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("App.xcodeproj")).unwrap();
        assert_eq!(detect_langs(t.path()), [Lang::Swift].into());
    }

    #[test]
    fn near_misses_do_not_match() {
        let t = tempfile::tempdir().unwrap();
        for f in [
            "requirements.md",
            "my-Cargo.toml",
            ".lua",
            "package.json.bak",
        ] {
            touch(t.path(), f);
        }
        assert!(detect_langs(t.path()).is_empty());
    }

    #[test]
    fn skips_vendored_ignored_and_deep_dirs() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "node_modules/x/Cargo.toml");
        touch(t.path(), "vendor-ignored/go.mod");
        touch(t.path(), ".hidden/Gemfile");
        touch(t.path(), "a/b/c/d/composer.json");
        std::fs::write(t.path().join(".gitignore"), "vendor-ignored/\n").unwrap();
        assert!(detect_langs(t.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let t = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        touch(outside.path(), "Cargo.toml");
        std::os::unix::fs::symlink(outside.path(), t.path().join("link")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("Cargo.toml"),
            t.path().join("Cargo.toml"),
        )
        .unwrap();
        assert!(detect_langs(t.path()).is_empty());
    }
}
