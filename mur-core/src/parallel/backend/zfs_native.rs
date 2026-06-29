#![allow(dead_code, unused_imports)]
use super::ParallelBackend;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ZfsNativeBackend {
    pub project_root: PathBuf,
}

impl ZfsNativeBackend {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

pub fn zfs_cli_available() -> bool {
    Command::new("zfs")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn is_on_zfs_pool(path: &Path) -> bool {
    dataset_for_path(path).is_ok()
}

pub fn parse_dataset(output: &str) -> Option<String> {
    let s = output.trim();
    if s.is_empty() || s == "-" {
        None
    } else {
        Some(s.to_string())
    }
}

/// Parse `zfs diff` output and strip the `track_mount` prefix, returning relative paths.
pub fn parse_diff_output(output: &str, track_mount: &Path) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(_, p)| PathBuf::from(p)))
        .filter_map(|p| p.strip_prefix(track_mount).ok().map(|r| r.to_path_buf()))
        .collect()
}

fn dataset_for_path(path: &Path) -> Result<String> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "name"])
        .arg(path)
        .output()
        .context("zfs get name")?;
    parse_dataset(&String::from_utf8_lossy(&out.stdout))
        .context("path is not on a ZFS dataset")
}

fn dataset_snapshot(dataset: &str, label: &str) -> Result<String> {
    let snap = format!("{dataset}@{label}");
    let st = Command::new("zfs")
        .args(["snapshot", &snap])
        .status()
        .context("zfs snapshot")?;
    if !st.success() {
        bail!("zfs snapshot {snap} failed");
    }
    Ok(snap)
}

fn dataset_clone(snap: &str, target_dataset: &str) -> Result<PathBuf> {
    let st = Command::new("zfs")
        .args(["clone", snap, target_dataset])
        .status()
        .context("zfs clone")?;
    if !st.success() {
        bail!("zfs clone failed: {snap} → {target_dataset}");
    }
    mountpoint_of(target_dataset)
}

fn mountpoint_of(dataset: &str) -> Result<PathBuf> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "mountpoint", dataset])
        .output()
        .context("zfs get mountpoint")?;
    if !out.status.success() {
        bail!("zfs get mountpoint failed for {dataset}");
    }
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    if p.as_os_str().is_empty() {
        bail!("empty mountpoint for {dataset}");
    }
    Ok(p)
}

impl ParallelBackend for ZfsNativeBackend {
    fn create_track(&self, name: &str) -> Result<PathBuf> {
        let base_dataset = dataset_for_path(&self.project_root)?;
        let snap =
            dataset_snapshot(&base_dataset, &format!("mur-parallel-base-{name}"))?;
        // Scope tracks under the project's own dataset to avoid cross-project collisions.
        let track_dataset = format!("{base_dataset}/mur-tracks/{name}");
        let mount = dataset_clone(&snap, &track_dataset)?;
        // Establish the base snapshot immediately so diff_files and promote work.
        dataset_snapshot(&track_dataset, "mur-base")?;
        Ok(mount)
    }

    fn base_snapshot(&self, track: &Path) -> Result<String> {
        let ds = dataset_for_path(track)?;
        Ok(format!("{ds}@mur-base"))
    }

    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>> {
        let ds = dataset_for_path(track)?;
        let out = Command::new("zfs")
            .args(["diff", since_snapshot, &ds])
            .output()
            .context("zfs diff")?;
        Ok(parse_diff_output(
            &String::from_utf8_lossy(&out.stdout),
            track,
        ))
    }

    fn promote(&self, track: &Path, target: &Path) -> Result<()> {
        let snap = self.base_snapshot(track)?;
        for rel in self.diff_files(track, &snap)? {
            let src = track.join(&rel);
            let dst = target.join(&rel);
            if src.is_file() {
                if let Some(p) = dst.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::copy(&src, &dst)?;
            }
        }
        Ok(())
    }

    fn destroy(&self, track: &Path) -> Result<()> {
        let ds = dataset_for_path(track)?;
        let st = Command::new("zfs")
            .args(["destroy", "-r", &ds])
            .status()
            .context("zfs destroy")?;
        if !st.success() {
            bail!("zfs destroy {ds} failed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_parses_pool_path() {
        assert_eq!(
            parse_dataset("pool/data/project\n"),
            Some("pool/data/project".to_string())
        );
    }

    #[test]
    fn dataset_rejects_empty_and_dash() {
        assert_eq!(parse_dataset(""), None);
        assert_eq!(parse_dataset("-\n"), None);
    }

    #[test]
    fn diff_output_extracts_modified_and_added() {
        let out = "M\t/pool/data/project/src/main.rs\n\
                   +\t/pool/data/project/src/new.rs\n\
                   -\t/pool/data/project/old.rs\n";
        let paths = parse_diff_output(out, Path::new("/pool/data/project"));
        // M, +, and - (deleted) are all included; promote skips missing files
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p == Path::new("src/main.rs")));
        assert!(paths.iter().any(|p| p == Path::new("src/new.rs")));
    }

    #[test]
    fn diff_output_strips_mount_prefix() {
        let out = "M\t/mnt/project/src/lib.rs\n";
        let paths = parse_diff_output(out, Path::new("/mnt/project"));
        assert_eq!(paths, vec![PathBuf::from("src/lib.rs")]);
    }
}
