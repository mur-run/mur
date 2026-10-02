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
