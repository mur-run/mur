use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn inbox_path(session_id: &str) -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("inbox")
        .join(format!("{session_id}.md"))
}

/// Write pre-computed context to the inbox file for a session.
pub fn write_inbox(path: &Path, content: &str) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

/// Read inbox content; returns None if missing or older than `max_age_secs`.
#[allow(dead_code)]
pub fn read_inbox(path: &Path, max_age_secs: u64) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let age = std::time::SystemTime::now().duration_since(modified).ok()?;
    if age.as_secs() >= max_age_secs {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_inbox_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        write_inbox(&path, "## context\n- foo — bar\n").unwrap();
        let content = read_inbox(&path, 300).unwrap();
        assert!(content.contains("foo"));
    }

    #[test]
    fn read_inbox_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.md");
        assert!(read_inbox(&path, 300).is_none());
    }

    #[test]
    fn read_inbox_stale_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.md");
        write_inbox(&path, "stale content").unwrap();
        assert!(read_inbox(&path, 0).is_none());
    }
}
