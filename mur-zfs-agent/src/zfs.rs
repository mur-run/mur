use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Reject names that could inject extra `zfs` args or escape the dataset
/// namespace. Allowlist mirrors ZFS's own component charset; a leading '-' is
/// refused so a name can never be parsed as a flag. (argv is not a shell, so
/// exploitability is low — but a privileged root daemon should still refuse it.)
pub fn validate_zfs_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        bail!("invalid zfs name length: {name:?}");
    }
    if name.starts_with('-') {
        bail!("zfs name may not start with '-': {name:?}");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
    {
        bail!("zfs name has invalid characters (allowed [A-Za-z0-9_.:-]): {name:?}");
    }
    Ok(())
}

/// A change-type-aware `zfs diff` entry.
#[derive(Debug, PartialEq, Eq)]
pub enum DiffChange {
    Modified,
    Added,
    Removed,
    Renamed,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DiffEntry {
    pub change: DiffChange,
    /// New path (for renames) or the changed path otherwise.
    pub path: PathBuf,
    /// Old path, only for renames.
    pub old_path: Option<PathBuf>,
}

/// Parse `zfs diff <snap> <dataset>` into change-type-aware entries. Handles
/// `M`/`+`/`-` (`<type>\t<path>`) and the rename form `R\t<old>\t<new>` — which
/// the old `split_once('\t')` mis-parsed into a single `old\tnew` PathBuf.
pub fn parse_zfs_diff_entries(output: &str) -> Vec<DiffEntry> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut f = line.split('\t');
            let ty = f.next()?;
            let fields: Vec<&str> = f.collect();
            match ty {
                "-" => fields.first().map(|p| DiffEntry {
                    change: DiffChange::Removed,
                    path: PathBuf::from(p),
                    old_path: None,
                }),
                "+" => fields.first().map(|p| DiffEntry {
                    change: DiffChange::Added,
                    path: PathBuf::from(p),
                    old_path: None,
                }),
                "R" if fields.len() >= 2 => Some(DiffEntry {
                    change: DiffChange::Renamed,
                    path: PathBuf::from(fields[1]),
                    old_path: Some(PathBuf::from(fields[0])),
                }),
                _ => fields.first().map(|p| DiffEntry {
                    change: DiffChange::Modified,
                    path: PathBuf::from(p),
                    old_path: None,
                }),
            }
        })
        .collect()
}

/// Back-compat: just the changed (new) paths.
pub fn parse_zfs_diff_output(output: &str) -> Vec<PathBuf> {
    parse_zfs_diff_entries(output)
        .into_iter()
        .map(|e| e.path)
        .collect()
}

/// Return the ZFS dataset name for the given mountpoint path.
pub fn dataset_for_path(path: &Path) -> Result<String> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "name"])
        .arg(path)
        .output()
        .context("zfs get name")?;
    if !out.status.success() {
        bail!("path not on ZFS: {path:?}");
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "-" {
        bail!("path not on ZFS: {path:?}");
    }
    Ok(s)
}

/// Clone `snap` as a new dataset `dataset` and return its mountpoint.
pub fn zfs_clone(snap: &str, dataset: &str) -> Result<PathBuf> {
    let st = Command::new("zfs")
        .args(["clone", snap, dataset])
        .status()
        .context("zfs clone")?;
    if !st.success() {
        bail!("zfs clone failed");
    }
    mountpoint_of(dataset)
}

/// Create a snapshot `dataset@label` and return the full snapshot name.
pub fn zfs_snapshot(dataset: &str, label: &str) -> Result<String> {
    let snap = format!("{dataset}@{label}");
    let st = Command::new("zfs")
        .args(["snapshot", &snap])
        .status()
        .context("zfs snapshot")?;
    if !st.success() {
        bail!("zfs snapshot failed: {snap}");
    }
    Ok(snap)
}

/// Return change-type-aware entries between `dataset@since_snap` and `dataset`.
pub fn zfs_diff_entries(dataset: &str, since_snap: &str) -> Result<Vec<DiffEntry>> {
    let since = format!("{dataset}@{since_snap}");
    let out = Command::new("zfs")
        .args(["diff", &since, dataset])
        .output()
        .context("zfs diff")?;
    if !out.status.success() {
        bail!("zfs diff failed for {since}");
    }
    Ok(parse_zfs_diff_entries(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

/// Apply a track's changes onto `target`, faithfully reflecting modifications,
/// additions, deletions, AND renames — not just additive copies (the previous
/// `if src.is_file()`-only loop silently dropped deletes/renames).
pub fn promote_track(track: &std::path::Path, target: &std::path::Path) -> Result<()> {
    let ds = dataset_for_path(track)?;
    for entry in zfs_diff_entries(&ds, "mur-base")? {
        // zfs diff yields absolute paths; strip the track mountpoint prefix.
        let rel = entry.path.strip_prefix(track).unwrap_or(&entry.path);
        let dst = target.join(rel);
        match entry.change {
            DiffChange::Removed => {
                let _ = std::fs::remove_file(&dst);
            }
            DiffChange::Renamed => {
                if let Some(old) = &entry.old_path {
                    let old_rel = old.strip_prefix(track).unwrap_or(old);
                    let _ = std::fs::remove_file(target.join(old_rel));
                }
                copy_file_into(&track.join(rel), &dst)?;
            }
            DiffChange::Modified | DiffChange::Added => {
                copy_file_into(&track.join(rel), &dst)?;
            }
        }
    }
    Ok(())
}

fn copy_file_into(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    if src.is_file() {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// Like `zfs_diff` but accepts a full snapshot ref (e.g. `pool/ds@mur-base`).
pub fn zfs_diff_ref(snap_ref: &str, dataset: &str) -> Result<Vec<PathBuf>> {
    let out = Command::new("zfs")
        .args(["diff", snap_ref, dataset])
        .output()
        .context("zfs diff")?;
    if !out.status.success() {
        bail!("zfs diff failed for {snap_ref}");
    }
    Ok(parse_zfs_diff_output(&String::from_utf8_lossy(&out.stdout)))
}

/// Promote a clone dataset (makes it independent of its origin).
#[allow(dead_code)]
pub fn zfs_promote(dataset: &str) -> Result<()> {
    let st = Command::new("zfs")
        .args(["promote", dataset])
        .status()
        .context("zfs promote")?;
    if !st.success() {
        bail!("zfs promote failed: {dataset}");
    }
    Ok(())
}

/// Recursively destroy a dataset and all its snapshots.
pub fn zfs_destroy(dataset: &str) -> Result<()> {
    let st = Command::new("zfs")
        .args(["destroy", "-r", dataset])
        .status()
        .context("zfs destroy")?;
    if !st.success() {
        bail!("zfs destroy failed: {dataset}");
    }
    Ok(())
}

/// Return the mountpoint of a ZFS dataset.
fn mountpoint_of(dataset: &str) -> Result<PathBuf> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "mountpoint", dataset])
        .output()
        .context("zfs get mountpoint")?;
    if !out.status.success() {
        bail!("zfs get mountpoint failed for {dataset}");
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "-" || s == "none" || s == "legacy" {
        bail!("no accessible mountpoint for {dataset}");
    }
    Ok(PathBuf::from(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_diff_empty() {
        assert!(parse_zfs_diff_output("").is_empty());
    }

    #[test]
    fn parse_diff_extracts_paths() {
        let out = "M\t/data/project/src/lib.rs\nM\t/data/project/Cargo.toml\n";
        let paths = parse_zfs_diff_output(out);
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("src/lib.rs")));
    }

    #[test]
    fn parse_diff_ignores_blank_lines() {
        let out = "\nM\t/data/file.rs\n\n";
        let paths = parse_zfs_diff_output(out);
        assert_eq!(paths.len(), 1);
    }

    #[test]
    fn rename_entry_keeps_new_path_clean() {
        let out = "R\t/data/old.rs\t/data/new.rs\n";
        let entries = parse_zfs_diff_entries(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].change, DiffChange::Renamed);
        assert_eq!(entries[0].path, PathBuf::from("/data/new.rs"));
        assert_eq!(entries[0].old_path, Some(PathBuf::from("/data/old.rs")));
        // Old bug: split_once('\t') produced a single "old\tnew" PathBuf.
        assert!(!entries[0].path.to_string_lossy().contains('\t'));
    }

    #[test]
    fn validate_zfs_name_allows_and_rejects() {
        assert!(validate_zfs_name("track-a").is_ok());
        assert!(validate_zfs_name("mur-parallel-base-1.2:3").is_ok());
        assert!(validate_zfs_name("").is_err(), "empty rejected");
        assert!(validate_zfs_name("-flag").is_err(), "leading dash rejected");
        assert!(validate_zfs_name("a b").is_err(), "space rejected");
        assert!(validate_zfs_name("a;rm -rf").is_err(), "metachars rejected");
        assert!(validate_zfs_name("a/b").is_err(), "slash rejected");
        assert!(
            validate_zfs_name(&"x".repeat(65)).is_err(),
            "over-long rejected"
        );
    }
}
