//! `mur agent mcp inspect --deep` — re-derive a vendored install from the
//! registry and diff it against what's on disk.
//!
//! ## Why this exists, when the lockfile pin already runs at every startup
//!
//! Every other check here compares the tree against a value stored **locally**
//! — `binary_sha256` and `lockfile_sha256` both live in `profile.yaml`, which
//! is writable by the same principal as the files they describe. That makes
//! them change detection: they catch software that swapped something without
//! telling you (a package manager re-resolving, an upgrade), not an adversary,
//! who would simply rewrite the recorded hash too.
//!
//! This check is the exception. It reinstalls from the pinned lockfile and
//! compares against **what the registry serves now**, so its reference value
//! is not on the machine being audited. It is the only thing here that can
//! catch a locally-edited `node_modules` even when the pin was edited to
//! match.
//!
//! That is also why it is a command and not a startup check: it costs a full
//! reinstall. Paying that at every agent start to defend against a threat the
//! other checks structurally cannot see would be the wrong trade — see #796.
//!
//! ## What it compares
//!
//! Regular files under `node_modules`, by content. Symlinks (npm's `.bin`
//! shims) are skipped: they are regenerated per install and their targets are
//! derived from the same package metadata already covered by the lockfile.
//! A tampered shim is therefore out of scope for this pass, and saying so is
//! better than implying a coverage that isn't there.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::Path;

/// One difference between the installed tree and a fresh install of the same
/// lockfile.
#[derive(Debug, PartialEq, Eq)]
pub enum Difference {
    /// Present on disk, absent from a clean install of this lockfile.
    Added(String),
    /// Absent from disk, present in a clean install.
    Removed(String),
    /// Present in both, different contents.
    Modified(String),
}

impl Difference {
    fn path(&self) -> &str {
        match self {
            Self::Added(p) | Self::Removed(p) | Self::Modified(p) => p,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Added(_) => "only on disk",
            Self::Removed(_) => "missing on disk",
            Self::Modified(_) => "contents differ",
        }
    }
}

/// Content hashes of every regular file under `root`, keyed by path relative
/// to `root`.
fn hash_tree(root: &Path) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    if !root.is_dir() {
        return Ok(out);
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))?;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            // symlink_metadata: never follow, so a symlinked directory can't
            // send the walk somewhere outside the tree.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                let hash = crate::cmd::agent_mcp_pin::compute_binary_sha256(&path)
                    .with_context(|| format!("hash {}", path.display()))?;
                out.insert(rel, hash);
            }
            // symlinks: skipped, see module docs
        }
    }
    Ok(out)
}

/// Compare two hashed trees. Deterministic order so output is diffable.
pub fn diff_trees(
    on_disk: &BTreeMap<String, String>,
    fresh: &BTreeMap<String, String>,
) -> Vec<Difference> {
    let mut diffs = Vec::new();
    for (path, hash) in on_disk {
        match fresh.get(path) {
            None => diffs.push(Difference::Added(path.clone())),
            Some(other) if other != hash => diffs.push(Difference::Modified(path.clone())),
            Some(_) => {}
        }
    }
    for path in fresh.keys() {
        if !on_disk.contains_key(path) {
            diffs.push(Difference::Removed(path.clone()));
        }
    }
    diffs.sort_by(|a, b| a.path().cmp(b.path()));
    diffs
}

/// Reinstall `install_dir`'s lockfile into `scratch` with `npm ci`.
fn reinstall_from_lockfile(install_dir: &Path, scratch: &Path) -> Result<()> {
    for f in ["package.json", "package-lock.json"] {
        let src = install_dir.join(f);
        std::fs::copy(&src, scratch.join(f))
            .with_context(|| format!("copy {} into the audit scratch dir", src.display()))?;
    }
    let out = std::process::Command::new("npm")
        .args([
            "ci",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--loglevel=error",
        ])
        .current_dir(scratch)
        .output()
        .map_err(|e| anyhow::anyhow!("run npm ci: {e} (is npm on PATH?)"))?;
    if !out.status.success() {
        bail!(
            "npm ci from the pinned lockfile failed: {}",
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }
    Ok(())
}

/// Re-derive the vendored tree from the registry and report what differs.
///
/// Returns the differences; an empty vec means the install matches a clean one
/// byte for byte.
pub fn audit_vendored_install(install_dir: &Path) -> Result<Vec<Difference>> {
    if !install_dir.join("package-lock.json").is_file() {
        bail!(
            "no lockfile at {} — nothing to re-derive from",
            install_dir.display(),
        );
    }
    let scratch = tempfile::tempdir().context("create audit scratch dir")?;
    reinstall_from_lockfile(install_dir, scratch.path())?;

    let on_disk = hash_tree(&install_dir.join("node_modules"))?;
    let fresh = hash_tree(&scratch.path().join("node_modules"))?;
    Ok(diff_trees(&on_disk, &fresh))
}

/// Render the audit for one entry. Returns true when the tree is clean.
pub fn report(server_name: &str, install_dir: &Path) -> bool {
    println!("Deep audit of `{server_name}` ({})", install_dir.display());
    println!("  reinstalling from the pinned lockfile…");
    match audit_vendored_install(install_dir) {
        Ok(diffs) if diffs.is_empty() => {
            println!("  status:         CLEAN — matches a fresh install of the same lockfile");
            true
        }
        Ok(diffs) => {
            println!(
                "  status:         TAMPERED — {} file(s) differ",
                diffs.len()
            );
            for d in diffs.iter().take(20) {
                println!("    {:<16} {}", d.label(), d.path());
            }
            if diffs.len() > 20 {
                println!("    … and {} more", diffs.len() - 20);
            }
            println!(
                "  hint:           `mur agent mcp vendor <agent> {server_name}` reinstalls a \
                 clean tree and re-records the pin.",
            );
            false
        }
        Err(e) => {
            // An audit that could not run is not a verdict either way, and must
            // not be reported as one.
            println!("  status:         NOT AUDITED — {e}");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(p, h)| (p.to_string(), h.to_string()))
            .collect()
    }

    #[test]
    fn an_identical_tree_has_no_differences() {
        let a = tree(&[("pkg/index.js", "aaa"), ("pkg/lib.js", "bbb")]);
        assert!(diff_trees(&a, &a).is_empty());
    }

    /// The case the whole check exists for: a file edited in place after
    /// install, which leaves the lockfile — and therefore the startup pin —
    /// completely untouched.
    #[test]
    fn an_edited_file_is_reported_as_modified() {
        let disk = tree(&[("pkg/index.js", "tampered")]);
        let fresh = tree(&[("pkg/index.js", "original")]);
        assert_eq!(
            diff_trees(&disk, &fresh),
            vec![Difference::Modified("pkg/index.js".into())],
        );
    }

    #[test]
    fn extra_and_missing_files_are_distinguished() {
        let disk = tree(&[("pkg/index.js", "a"), ("pkg/backdoor.js", "x")]);
        let fresh = tree(&[("pkg/index.js", "a"), ("pkg/helper.js", "h")]);
        assert_eq!(
            diff_trees(&disk, &fresh),
            vec![
                Difference::Added("pkg/backdoor.js".into()),
                Difference::Removed("pkg/helper.js".into()),
            ],
            "an injected file and a deleted one are different findings",
        );
    }

    #[test]
    fn output_order_is_deterministic() {
        let disk = tree(&[("z", "1"), ("a", "1"), ("m", "2")]);
        let fresh = tree(&[("m", "3")]);
        let diffs = diff_trees(&disk, &fresh);
        let paths: Vec<&str> = diffs.iter().map(|d| d.path()).collect();
        assert_eq!(paths, vec!["a", "m", "z"], "sorted so runs are comparable");
    }

    #[test]
    fn hash_tree_walks_nested_dirs_and_skips_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let deep = d.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("f.js"), b"content").unwrap();
        std::fs::write(d.path().join("top.js"), b"top").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(d.path().join("top.js"), d.path().join("link.js")).unwrap();

        let t = hash_tree(d.path()).unwrap();
        assert!(t.contains_key("a/b/c/f.js"), "nested files must be walked");
        assert!(t.contains_key("top.js"));
        #[cfg(unix)]
        assert!(
            !t.contains_key("link.js"),
            "symlinks are regenerated per install; see module docs",
        );
    }

    #[test]
    fn a_missing_tree_hashes_to_empty_rather_than_erroring() {
        let d = tempfile::tempdir().unwrap();
        assert!(
            hash_tree(&d.path().join("node_modules"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn auditing_without_a_lockfile_is_an_error_not_a_clean_verdict() {
        let d = tempfile::tempdir().unwrap();
        let err = audit_vendored_install(d.path()).unwrap_err().to_string();
        assert!(err.contains("no lockfile"), "got: {err}");
    }
}
