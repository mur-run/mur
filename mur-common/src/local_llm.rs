//! Location and accessors for the bundled local-model base URL.
//!
//! Hub starts an MLX inference server on an ephemeral port and writes its
//! OpenAI-compatible base URL here. Agents — started by launchd and therefore
//! NOT inheriting Hub's environment — read it back from this file.

use std::path::{Path, PathBuf};

/// HuggingFace repo holding bundled-free local model.
pub const DEFAULT_LOCAL_MODEL_REPO: &str = "mlx-community/Qwen3.5-2B-MLX-4bit";

/// Directory name (under `<mur_home>/models/`).
pub const DEFAULT_LOCAL_MODEL_DIR: &str = "Qwen3.5-2B-MLX-4bit";

/// Path to the local model directory under `<mur_home>/models/<model_dir>`.
pub fn local_model_dir(mur_home: &Path, model_dir: &str) -> PathBuf {
    mur_home.join("models").join(model_dir)
}

/// Path to the model-complete marker file (`.complete`) within the model directory.
pub fn model_complete_marker(model_dir: &Path) -> PathBuf {
    model_dir.join(".complete")
}

/// Path to the file holding the local model base URL, under `<mur_home>`.
pub fn base_url_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("local_llm.url")
}

/// Atomically write the base URL (temp file + rename).
pub fn write_base_url(mur_home: &Path, url: &str) -> std::io::Result<()> {
    let path = base_url_path(mur_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("url.tmp");
    std::fs::write(&tmp, url.as_bytes())?;
    std::fs::rename(&tmp, &path)
}

/// Read the base URL, trimming whitespace. `None` if absent/empty.
pub fn read_base_url(mur_home: &Path) -> Option<String> {
    let s = std::fs::read_to_string(base_url_path(mur_home)).ok()?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_then_read_roundtrips() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(read_base_url(tmp.path()), None);
        write_base_url(tmp.path(), "http://127.0.0.1:50321/v1").unwrap();
        assert_eq!(
            read_base_url(tmp.path()),
            Some("http://127.0.0.1:50321/v1".to_string())
        );
    }

    #[test]
    fn blank_file_reads_as_none() {
        let tmp = TempDir::new().unwrap();
        write_base_url(tmp.path(), "   \n").unwrap();
        assert_eq!(read_base_url(tmp.path()), None);
    }

    #[test]
    fn model_dir_and_marker_paths() {
        let home = Path::new("/tmp/murhome");
        let dir = local_model_dir(home, DEFAULT_LOCAL_MODEL_DIR);
        assert_eq!(dir, home.join("models").join("Qwen3.5-2B-MLX-4bit"));
        assert_eq!(model_complete_marker(&dir), dir.join(".complete"));
    }
}
