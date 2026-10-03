//! `mur agent doctor` scratch-dir size line (spec §Cleanup, `scratch.warn_size_mb`).

use std::path::Path;

const BYTES_PER_MB: u64 = 1024 * 1024;

/// Size of the scratch tree and whether it crossed `warn_size_mb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchUsage {
    pub bytes: u64,
    pub over_warn: bool,
}

/// `None` when the dir does not exist yet (agent never started).
pub fn scratch_usage(dir: &Path, warn_size_mb: u64) -> Option<ScratchUsage> {
    if !dir.is_dir() {
        return None;
    }
    let bytes = tree_bytes(dir);
    Some(ScratchUsage {
        bytes,
        over_warn: bytes > warn_size_mb.saturating_mul(BYTES_PER_MB),
    })
}

/// Sum of regular-file sizes; links are never followed, unreadable
/// entries count as zero (doctor is best-effort).
fn tree_bytes(p: &Path) -> u64 {
    let Ok(md) = std::fs::symlink_metadata(p) else {
        return 0;
    };
    if md.is_file() {
        return md.len();
    }
    if !md.is_dir() {
        return 0;
    }
    std::fs::read_dir(p)
        .map(|rd| rd.flatten().map(|e| tree_bytes(&e.path())).sum())
        .unwrap_or(0)
}

/// One doctor line, or `None` when there is nothing to report.
pub fn scratch_line(name: &str, dir: &Path, warn_size_mb: u64) -> Option<String> {
    let u = scratch_usage(dir, warn_size_mb)?;
    let mb = u.bytes as f64 / BYTES_PER_MB as f64;
    Some(if u.over_warn {
        format!(
            "  {name}: scratch {} is {mb:.1} MB (over warn_size_mb {warn_size_mb}) \u{2192} clear it or lower scratch.retention_days",
            dir.display()
        )
    } else {
        format!("  {name}: scratch {} is {mb:.1} MB", dir.display())
    })
}

/// Scratch lines for installed agents with no `running.lock` — the
/// running ones are already reported in the per-agent section. Sorted by
/// name; agents whose scratch dir does not exist are skipped.
pub fn stopped_scratch_lines(mur_home: &Path, warn_size_mb: u64) -> Vec<String> {
    let agents_dir = mur_home.join("agents");
    let Ok(rd) = std::fs::read_dir(&agents_dir) else {
        return Vec::new();
    };
    let mut names: Vec<_> = rd
        .flatten()
        .filter(|e| e.path().is_dir() && !e.path().join("running.lock").exists())
        .map(|e| e.file_name())
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|n| {
            let dir =
                mur_agent_runtime::agent_paths::agent_scratch_dir(&agents_dir.join(&n)).ok()?;
            let label = format!("{} (stopped)", n.to_string_lossy());
            scratch_line(&label, &dir, warn_size_mb)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_dir_reports_nothing() {
        let t = tempfile::TempDir::new().unwrap();
        assert!(scratch_line("a", &t.path().join("nope"), 1).is_none());
    }

    #[test]
    fn size_counted_and_warns_only_above_threshold() {
        let t = tempfile::TempDir::new().unwrap();
        let d = t.path().join("s");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/big"), vec![0u8; (BYTES_PER_MB + 1) as usize]).unwrap();
        let u = scratch_usage(&d, 1).unwrap();
        assert_eq!(u.bytes, BYTES_PER_MB + 1);
        assert!(u.over_warn);
        assert!(!scratch_usage(&d, 2).unwrap().over_warn);
        let line = scratch_line("a", &d, 1).unwrap();
        assert!(line.contains("over warn_size_mb 1"), "{line}");
        assert!(!scratch_line("a", &d, 2).unwrap().contains("over"));
    }

    #[test]
    fn stopped_agents_reported_running_ones_skipped() {
        let t = tempfile::TempDir::new().unwrap();
        let home = t.path();
        for n in ["run", "stop", "fresh"] {
            std::fs::create_dir_all(home.join("agents").join(n)).unwrap();
        }
        std::fs::write(home.join("agents/run/running.lock"), "").unwrap();
        for n in ["run", "stop"] {
            std::fs::create_dir_all(home.join("tmp").join(n)).unwrap();
        }
        std::fs::write(home.join("tmp/stop/f"), vec![0u8; 10]).unwrap();
        let lines = stopped_scratch_lines(home, 1);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("stop (stopped)"), "{lines:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let t = tempfile::TempDir::new().unwrap();
        let outside = t.path().join("outside");
        std::fs::write(&outside, vec![0u8; 4096]).unwrap();
        let d = t.path().join("s");
        std::fs::create_dir_all(&d).unwrap();
        std::os::unix::fs::symlink(&outside, d.join("ln")).unwrap();
        assert_eq!(scratch_usage(&d, 1).unwrap().bytes, 0);
    }
}
