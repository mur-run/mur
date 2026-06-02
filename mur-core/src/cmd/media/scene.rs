//! scene-explain: capture the current VLC frame and explain it with the local
//! multimodal model.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Return the most recently modified regular file in `dir`, if any.
pub fn newest_file(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let mtime = entry.metadata().ok()?.modified().ok()?;
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn newest_file_picks_latest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.png"), b"a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("b.png"), b"b").unwrap();
        assert_eq!(
            newest_file(dir.path()).unwrap().file_name().unwrap(),
            "b.png"
        );
    }

    #[test]
    fn newest_file_empty_dir_is_none() {
        let dir = TempDir::new().unwrap();
        assert!(newest_file(dir.path()).is_none());
    }
}
