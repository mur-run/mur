mod dir_handle;

use std::path::{Path, PathBuf};

use dir_handle::DirHandle;

pub struct ScannedFile {
    pub relative_path: String,
    pub content: String,
    pub language: String,
}

pub(crate) const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "build",
    "dist",
    "__pycache__",
    ".venv",
    "vendor",
    ".next",
    ".nuxt",
    ".turbo",
    ".svelte-kit",
    "Pods",
    ".build",
    "DerivedData",
];

fn language_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "go" => Some("go"),
        "py" => Some("python"),
        "swift" => Some("swift"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        "scala" => Some("scala"),
        "ex" | "exs" => Some("elixir"),
        "zig" => Some("zig"),
        "lua" => Some("lua"),
        "sh" | "bash" | "zsh" => Some("shell"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "json" => Some("json"),
        "md" => Some("markdown"),
        "sql" => Some("sql"),
        "proto" => Some("protobuf"),
        "tf" => Some("terraform"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        _ => None,
    }
}

/// Directory nesting the walk descends into before it stops. A bound on
/// recursion, not a tuning knob: real source trees sit far below it.
pub(crate) const MAX_WALK_DEPTH: usize = 64;

/// Scans `project_path`, which on unix must be absolute and symlink-free
/// (indexed roots are stored canonicalized); otherwise the scan is empty.
/// See [`DirHandle::open_root`].
pub fn scan_project(project_path: &Path) -> Vec<ScannedFile> {
    let mut files = Vec::new();
    if let Some(dir) = DirHandle::open_root(project_path) {
        let gitignore = load_gitignore_at(&dir);
        walk_dir(
            &dir,
            project_path,
            project_path,
            0,
            &gitignore,
            &mut |_| {},
            &mut files,
        );
    }
    files
}

/// What a directory entry is, judged from the entry itself, never its target.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File,
    Other,
}

/// Walks `dir` without ever following a symlink. The index can be built by a
/// process with wider filesystem access than whoever planted the tree (a
/// sandboxed agent's commit triggers it), so a link pointing out of the repo
/// would read files the planter could not, and `a -> .` would recurse forever.
///
/// Every open goes through the parent's [`DirHandle`], not a path, so
/// swapping any directory on the way for a symlink after it was classified
/// cannot redirect the walk: the handle already pins the real directory.
/// `path` is only the logical location, used for ignore rules and output.
/// `before_open` runs just before each open (tests use it to race the walk).
fn walk_dir(
    dir: &DirHandle,
    root: &Path,
    path: &Path,
    depth: usize,
    gitignore_patterns: &[String],
    before_open: &mut dyn FnMut(&str),
    out: &mut Vec<ScannedFile>,
) {
    if depth >= MAX_WALK_DEPTH {
        return;
    }
    let mut entries = dir.entries();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    for (file_name, kind) in entries {
        let name = file_name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let child = path.join(&file_name);
        let rel = relative_path(root, &child);

        match kind {
            EntryKind::Dir => {
                if SKIP_DIRS.iter().any(|&s| name == s)
                    || is_gitignored(&rel, gitignore_patterns, true)
                {
                    continue;
                }
                before_open(&rel);
                if let Some(sub) = dir.open_dir(&file_name) {
                    walk_dir(
                        &sub,
                        root,
                        &child,
                        depth + 1,
                        gitignore_patterns,
                        before_open,
                        out,
                    );
                }
            }
            EntryKind::File => {
                if is_gitignored(&rel, gitignore_patterns, false) {
                    continue;
                }
                let ext = child.extension().and_then(|e| e.to_str()).unwrap_or("");
                let Some(language) = language_for_ext(ext) else {
                    continue;
                };
                before_open(&rel);
                let Some(bytes) = dir.read_file(&file_name) else {
                    continue;
                };
                let check_len = bytes.len().min(512);
                if bytes[..check_len].contains(&0) {
                    continue;
                }
                let Ok(content) = String::from_utf8(bytes) else {
                    continue;
                };
                out.push(ScannedFile {
                    relative_path: rel,
                    content,
                    language: language.to_string(),
                });
            }
            EntryKind::Other => {}
        }
    }
}

/// Root `.gitignore` patterns for a caller holding only a path (the code-nav
/// wizard, which runs as the user). Canonicalizes first, so a root reached
/// through a link still works there; the file itself is still opened
/// `O_NOFOLLOW` through the root handle.
pub(crate) fn load_gitignore(project_path: &Path) -> Vec<String> {
    project_path
        .canonicalize()
        .ok()
        .and_then(|p| DirHandle::open_root(&p))
        .map(|dir| load_gitignore_at(&dir))
        .unwrap_or_default()
}

/// Root `.gitignore` patterns, read through the already-open root handle
/// (`O_NOFOLLOW`, size-capped), so a `.gitignore` symlinked out of the repo
/// is not read and yields no patterns.
fn load_gitignore_at(root: &DirHandle) -> Vec<String> {
    let Some(content) = root
        .read_file(std::ffi::OsStr::new(".gitignore"))
        .and_then(|bytes| String::from_utf8(bytes).ok())
    else {
        return Vec::new();
    };
    content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.strip_prefix('/').unwrap_or(l).to_string())
        .map(|l| l.strip_suffix('/').unwrap_or(&l).to_string())
        .collect()
}

pub(crate) fn is_gitignored(rel_path: &str, patterns: &[String], is_dir: bool) -> bool {
    for pattern in patterns {
        if rel_path == *pattern || rel_path.starts_with(&format!("{pattern}/")) {
            return true;
        }
        if !pattern.contains('/') {
            let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
            if file_name == *pattern {
                return true;
            }
            if is_dir {
                for component in rel_path.split('/') {
                    if component == *pattern {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Project name = the codebase-index key (`<name>.lance`).
/// Index key for a path. The PRIMARY working tree keeps the bare repo basename
/// (e.g. `mur`); each LINKED git worktree gets its own key
/// `{repo}--{worktree_basename}-{hash8}` so divergent branches index into
/// separate, isolated slots instead of thrashing one shared index.
pub fn project_name_from_path(path: &Path) -> String {
    let main_root = git_main_repo_root(path);
    let wt_root = git_worktree_root(path);

    let base = main_root
        .as_deref()
        .or(wt_root.as_deref())
        .unwrap_or(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());

    match (main_root.as_deref(), wt_root.as_deref()) {
        // Linked worktree: its own root differs from the main repo root.
        (Some(main), Some(wt)) if main != wt => {
            let wt_base = wt
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "wt".to_string());
            let hash = blake3::hash(wt.to_string_lossy().as_bytes()).to_hex();
            format!("{base}--{wt_base}-{}", &hash[..8])
        }
        // Primary worktree (or non-git path): unchanged, back-compatible key.
        _ => base,
    }
}

/// Main repo root for any path inside a git repo (incl. linked worktrees).
/// Public so the index can record it in metadata for the orphan-sweep guard.
pub fn main_repo_root(path: &Path) -> Option<PathBuf> {
    git_main_repo_root(path)
}

/// This worktree's OWN top-level root (`git rev-parse --show-toplevel`).
/// For the primary worktree this equals `git_main_repo_root`; for a linked
/// worktree it is the worktree dir. `None` when not in a repo / git missing.
fn git_worktree_root(path: &Path) -> Option<PathBuf> {
    let out = mur_common::repo_walk::bound_git(&mut std::process::Command::new("git"))
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let top = std::str::from_utf8(&out.stdout).ok()?.trim();
    if top.is_empty() {
        return None;
    }
    Some(PathBuf::from(top))
}

/// Main working-tree root for any path inside a git repo (including linked
/// worktrees), via `git --git-common-dir`. `None` when `path` isn't in a repo or
/// git is unavailable — callers fall back to the path's own basename.
fn git_main_repo_root(path: &Path) -> Option<PathBuf> {
    // ponytail: shell out — avoids threading a git2 Repository through this pure
    // path helper; one subprocess per CLI invocation is negligible.
    let out = mur_common::repo_walk::bound_git(&mut std::process::Command::new("git"))
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `--git-common-dir` points at `<main-repo>/.git`; its parent is the root.
    let common = PathBuf::from(std::str::from_utf8(&out.stdout).ok()?.trim());
    common.parent().map(Path::to_path_buf)
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_language_detection() {
        assert_eq!(language_for_ext("rs"), Some("rust"));
        assert_eq!(language_for_ext("ts"), Some("typescript"));
        assert_eq!(language_for_ext("py"), Some("python"));
        assert_eq!(language_for_ext("xyz"), None);
    }

    #[test]
    fn test_gitignore_matching() {
        let patterns = vec!["dist".to_string(), "*.log".to_string()];
        assert!(is_gitignored("dist", &patterns, true));
        assert!(is_gitignored("dist/index.js", &patterns, false));
        assert!(!is_gitignored("src/main.rs", &patterns, false));
    }

    #[test]
    fn test_project_name() {
        // Non-git path falls back to its own basename.
        let path = PathBuf::from("/home/user/Projects/my-app");
        assert_eq!(project_name_from_path(&path), "my-app");
    }

    /// A tempdir and its canonical path. The scanner requires a symlink-free
    /// root (production stores `project_path` canonicalized), and macOS's
    /// default temp dir sits under the `/var -> private/var` link.
    fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        (tmp, base)
    }

    #[cfg(unix)]
    fn symlink_fixture() -> (tempfile::TempDir, PathBuf) {
        let (tmp, base) = canonical_tempdir();
        let outside = base.join("outside");
        let repo = base.join("repo");
        fs::create_dir_all(outside.join("secrets")).unwrap();
        fs::write(outside.join("secret.toml"), "token = \"leak\"\n").unwrap();
        fs::write(outside.join("secrets/key.json"), "{\"k\":\"leak\"}\n").unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
        (tmp, repo)
    }

    fn paths(files: &[ScannedFile]) -> Vec<&str> {
        files.iter().map(|f| f.relative_path.as_str()).collect()
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_file_outside_root_is_not_indexed() {
        let (tmp, repo) = symlink_fixture();
        std::os::unix::fs::symlink(
            tmp.path().join("outside/secret.toml"),
            repo.join("cfg.toml"),
        )
        .unwrap();
        let files = scan_project(&repo);
        assert_eq!(paths(&files), ["src/main.rs"]);
        assert!(files.iter().all(|f| !f.content.contains("leak")));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_dir_outside_root_is_not_followed() {
        let (tmp, repo) = symlink_fixture();
        std::os::unix::fs::symlink(tmp.path().join("outside/secrets"), repo.join("vault")).unwrap();
        let files = scan_project(&repo);
        assert_eq!(paths(&files), ["src/main.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn self_referencing_symlink_loop_terminates_without_duplicates() {
        let (_tmp, repo) = symlink_fixture();
        std::os::unix::fs::symlink(".", repo.join("a")).unwrap();
        std::os::unix::fs::symlink("..", repo.join("src/up")).unwrap();
        let files = scan_project(&repo);
        assert_eq!(paths(&files), ["src/main.rs"]);
    }

    /// Scans `repo`, and the moment the walk has classified `trigger` but
    /// not yet opened it, swaps `repo/src` for a symlink to an outside
    /// directory holding a `main.rs` full of secrets.
    #[cfg(unix)]
    fn scan_with_src_swapped_at(trigger: &str) -> Vec<ScannedFile> {
        let (tmp, repo) = symlink_fixture();
        let outside = tmp.path().join("outside");
        fs::write(outside.join("main.rs"), "const K: &str = \"leak\";\n").unwrap();
        let gitignore = load_gitignore(&repo);
        let mut swapped = false;
        let mut files = Vec::new();
        let mut swap = |rel: &str| {
            if rel == trigger && !swapped {
                swapped = true;
                fs::rename(repo.join("src"), repo.join("src-moved")).unwrap();
                std::os::unix::fs::symlink(&outside, repo.join("src")).unwrap();
            }
        };
        let dir = DirHandle::open_root(&repo).unwrap();
        walk_dir(&dir, &repo, &repo, 0, &gitignore, &mut swap, &mut files);
        assert!(swapped, "walk never reached {trigger}");
        files
    }

    #[cfg(unix)]
    #[test]
    fn dir_swapped_for_symlink_before_descent_is_not_followed() {
        let files = scan_with_src_swapped_at("src");
        assert!(files.iter().all(|f| !f.content.contains("leak")));
    }

    #[cfg(unix)]
    #[test]
    fn parent_swapped_for_symlink_before_file_read_is_not_followed() {
        let files = scan_with_src_swapped_at("src/main.rs");
        assert!(files.iter().all(|f| !f.content.contains("leak")));
        assert_eq!(paths(&files), ["src/main.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn root_that_is_a_symlink_scans_empty() {
        let (tmp, repo) = symlink_fixture();
        let link = tmp.path().canonicalize().unwrap().join("repo-link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        assert!(scan_project(&link).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn root_below_a_symlinked_ancestor_scans_empty() {
        let (tmp, _repo) = symlink_fixture();
        let base = tmp.path().canonicalize().unwrap();
        // `alias -> .`, so `alias/repo` names the real repo through a link.
        std::os::unix::fs::symlink(".", base.join("alias")).unwrap();
        assert!(scan_project(&base.join("alias/repo")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn non_canonical_root_paths_scan_empty() {
        let (_tmp, repo) = symlink_fixture();
        assert!(scan_project(&repo.join("src/..")).is_empty());
        assert!(scan_project(Path::new("relative/repo")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_root_scans_as_before() {
        let (_tmp, repo) = symlink_fixture();
        fs::write(repo.join("src/gen.rs"), "fn g() {}\n").unwrap();
        fs::write(repo.join("lib.py"), "x = 1\n").unwrap();
        fs::write(repo.join(".gitignore"), "# generated\n/src/gen.rs\n").unwrap();
        assert_eq!(load_gitignore(&repo), ["src/gen.rs"]);
        assert_eq!(paths(&scan_project(&repo)), ["lib.py", "src/main.rs"]);
    }

    #[cfg(unix)]
    #[test]
    fn gitignore_symlinked_out_of_repo_is_not_read() {
        let (tmp, repo) = symlink_fixture();
        let outside = tmp.path().canonicalize().unwrap().join("outside/ignore");
        fs::write(&outside, "src\n").unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(".gitignore")).unwrap();
        assert!(load_gitignore(&repo).is_empty());
        assert_eq!(paths(&scan_project(&repo)), ["src/main.rs"]);
    }

    #[test]
    fn walk_stops_at_max_depth() {
        let (_tmp, base) = canonical_tempdir();
        let mut deep = base.clone();
        for i in 0..=MAX_WALK_DEPTH {
            deep.push(format!("d{i}"));
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("too_deep.rs"), "fn x() {}\n").unwrap();
        fs::write(base.join("d0/shallow.rs"), "fn y() {}\n").unwrap();
        let files = scan_project(&base);
        assert_eq!(paths(&files), ["d0/shallow.rs"]);
    }

    #[test]
    fn linked_worktrees_get_distinct_keys() {
        use std::process::Command;
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir(&repo).unwrap();

        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        let wt1 = tmp.path().join("wt-foo");
        let wt2 = tmp.path().join("wt-bar");
        git(&["worktree", "add", "-q", "-b", "b1", wt1.to_str().unwrap()]);
        git(&["worktree", "add", "-q", "-b", "b2", wt2.to_str().unwrap()]);

        // Option 2: primary tree keeps the bare repo name; each linked worktree
        // gets its OWN namespaced key so divergent branches index into separate
        // slots instead of thrashing one shared index.
        let primary = project_name_from_path(&repo);
        let k1 = project_name_from_path(&wt1);
        let k2 = project_name_from_path(&wt2);
        assert_eq!(primary, "myrepo", "primary keeps bare repo name");
        assert!(k1.starts_with("myrepo--"), "worktree namespaced: {k1}");
        assert_ne!(k1, primary);
        assert_ne!(k1, k2, "distinct worktrees → distinct keys");
    }
}
