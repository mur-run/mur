use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Parse the output of `zfs diff <snap> <dataset>` into a list of changed paths.
/// Each line is `<change_type>\t<path>` where change_type is one of M, +, -, R.
/// Blank lines are ignored.
pub fn parse_zfs_diff_output(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_once('\t').map(|(_, path)| PathBuf::from(path)))
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

/// Return changed paths between `dataset@since_snap` and `dataset`.
pub fn zfs_diff(dataset: &str, since_snap: &str) -> Result<Vec<PathBuf>> {
    let since = format!("{dataset}@{since_snap}");
    let out = Command::new("zfs")
        .args(["diff", &since, dataset])
        .output()
        .context("zfs diff")?;
    if !out.status.success() {
        bail!("zfs diff failed for {since}");
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
}
