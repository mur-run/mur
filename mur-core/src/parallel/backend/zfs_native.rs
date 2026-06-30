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

/// A single change-type-aware `zfs diff` entry.
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
    /// Relative path (the NEW path for renames).
    pub path: PathBuf,
    /// Relative OLD path, present only for renames.
    pub old_path: Option<PathBuf>,
}

/// Parse `zfs diff` output into change-type-aware entries, stripping the
/// `track_mount` prefix. Handles `M`/`+`/`-` (`<type>\t<path>`) and the rename
/// form `R\t<old>\t<new>` — which the old `split_once('\t')` mis-parsed into a
/// single `old\tnew` PathBuf, silently corrupting the promoted path.
pub fn parse_diff_entries(output: &str, track_mount: &Path) -> Vec<DiffEntry> {
    let rel = |p: &str| {
        Path::new(p)
            .strip_prefix(track_mount)
            .map(|r| r.to_path_buf())
            .unwrap_or_else(|_| PathBuf::from(p))
    };
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut f = line.split('\t');
            let ty = f.next()?;
            let fields: Vec<&str> = f.collect();
            match ty {
                "-" => fields.first().map(|p| DiffEntry {
                    change: DiffChange::Removed,
                    path: rel(p),
                    old_path: None,
                }),
                "+" => fields.first().map(|p| DiffEntry {
                    change: DiffChange::Added,
                    path: rel(p),
                    old_path: None,
                }),
                "R" if fields.len() >= 2 => Some(DiffEntry {
                    change: DiffChange::Renamed,
                    path: rel(fields[1]),
                    old_path: Some(rel(fields[0])),
                }),
                // "M", a lone "R", or any other type → treat as modified.
                _ => fields.first().map(|p| DiffEntry {
                    change: DiffChange::Modified,
                    path: rel(p),
                    old_path: None,
                }),
            }
        })
        .collect()
}

/// Back-compat: just the changed (new) relative paths, for `diff_files`.
pub fn parse_diff_output(output: &str, track_mount: &Path) -> Vec<PathBuf> {
    parse_diff_entries(output, track_mount)
        .into_iter()
        .map(|e| e.path)
        .collect()
}

/// Copy `src` → `dst` (creating parents) when `src` is a regular file.
fn copy_file_into(src: &Path, dst: &Path) -> Result<()> {
    if src.is_file() {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

fn dataset_for_path(path: &Path) -> Result<String> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "name"])
        .arg(path)
        .output()
        .context("zfs get name")?;
    parse_dataset(&String::from_utf8_lossy(&out.stdout)).context("path is not on a ZFS dataset")
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
        let snap = dataset_snapshot(&base_dataset, &format!("mur-parallel-base-{name}"))?;
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
        // Faithfully reflect the track's state — including deletions and renames,
        // not just additive copies (the previous `if src.is_file()`-only path
        // silently dropped both).
        let snap = self.base_snapshot(track)?;
        let ds = dataset_for_path(track)?;
        let out = Command::new("zfs")
            .args(["diff", &snap, &ds])
            .output()
            .context("zfs diff")?;
        for entry in parse_diff_entries(&String::from_utf8_lossy(&out.stdout), track) {
            let dst = target.join(&entry.path);
            match entry.change {
                DiffChange::Removed => {
                    let _ = std::fs::remove_file(&dst);
                }
                DiffChange::Renamed => {
                    if let Some(old) = &entry.old_path {
                        let _ = std::fs::remove_file(target.join(old));
                    }
                    copy_file_into(&track.join(&entry.path), &dst)?;
                }
                DiffChange::Modified | DiffChange::Added => {
                    copy_file_into(&track.join(&entry.path), &dst)?;
                }
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

    #[test]
    fn diff_entries_carry_change_type_and_rename_pair() {
        let mount = Path::new("/wt");
        let out = "M\t/wt/src/a.rs\n\
                   +\t/wt/src/new.rs\n\
                   -\t/wt/src/gone.rs\n\
                   R\t/wt/src/old.rs\t/wt/src/renamed.rs\n";
        let e = parse_diff_entries(out, mount);
        assert_eq!(e.len(), 4);
        assert_eq!(e[0].change, DiffChange::Modified);
        assert_eq!(e[1].change, DiffChange::Added);
        assert_eq!(e[2].change, DiffChange::Removed);
        assert_eq!(
            e[3],
            DiffEntry {
                change: DiffChange::Renamed,
                path: PathBuf::from("src/renamed.rs"),
                old_path: Some(PathBuf::from("src/old.rs")),
            }
        );
        // The rename's NEW path must be clean — never the old `old\tnew` blob.
        assert!(!e[3].path.to_string_lossy().contains('\t'));
    }
}
