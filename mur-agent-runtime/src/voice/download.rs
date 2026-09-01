//! Model download with SHA-256 integrity check.
//!
//! This module is used by `mur-core` (CLI) to download voice model weights.
//! It deliberately rejects non-`file://` URLs when called from within
//! `mur-agent-runtime` to enforce the privacy invariant that no audio
//! or data leaves the device at runtime. Downloads are a one-time setup
//! step initiated by the user from the CLI.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

/// Static spec describing a downloadable model file.
pub struct ModelSpec {
    /// File name (without directory).
    pub name: &'static str,
    /// Full HTTPS URL (for CLI download) or `file://` URL (for tests).
    pub url: &'static str,
    /// Lowercase hex SHA-256 of the expected file content.
    pub sha256: &'static str,
    /// Subdirectory under `~/.mur/models/` where file is cached.
    pub subdir: &'static str,
}

/// Ensure model file is present and its SHA-256 matches `spec.sha256`.
///
/// - Already present + hash matches → returns immediately with the path.
/// - Present but hash mismatches → deletes and re-downloads.
/// - Missing → downloads to `.tmp`, verifies hash, atomically renames.
///
/// `on_progress(downloaded_bytes, total_bytes)` is called periodically
/// during download. Pass `|_, _| {}` to suppress.
///
/// # Network safety
///
/// At runtime (inside `mur-agent-runtime`), only `file://` URLs are
/// accepted. HTTPS downloads must be initiated from `mur-core`
/// (`cmd::agent_voice::cmd_voice_download`). This enforces the privacy
/// invariant: the runtime never opens network connections.
pub fn ensure_model(
    mur_root: &Path,
    spec: &ModelSpec,
    on_progress: impl Fn(u64, u64),
) -> Result<PathBuf> {
    let target_dir = mur_root.join("models").join(spec.subdir);
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("create models dir {}", target_dir.display()))?;

    let final_path = target_dir.join(spec.name);

    if final_path.exists() {
        let actual = sha256_of_file(&final_path)?;
        if actual == spec.sha256 {
            return Ok(final_path);
        }
        tracing::warn!(
            name = spec.name,
            expected = spec.sha256,
            actual = %actual,
            "sha256 mismatch on cached file; re-downloading"
        );
        std::fs::remove_file(&final_path)?;
    }

    let tmp_path = final_path.with_extension("tmp");
    download_to(&tmp_path, spec.url, on_progress)?;

    let actual = sha256_of_file(&tmp_path)?;
    if actual != spec.sha256 {
        std::fs::remove_file(&tmp_path).ok();
        bail!(
            "sha256 mismatch after download of '{}': expected {}, got {}",
            spec.name,
            spec.sha256,
            actual
        );
    }

    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {} → {}", tmp_path.display(), final_path.display()))?;

    Ok(final_path)
}

fn sha256_of_file(path: &Path) -> Result<String> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::with_capacity(65536, f);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Download URL content to `dest`.
///
/// Only `file://` URLs are accepted here (runtime safety). HTTPS downloads
/// are handled by the CLI via reqwest before the runtime is ever invoked.
fn download_to(dest: &Path, url: &str, on_progress: impl Fn(u64, u64)) -> Result<()> {
    if let Some(local) = url.strip_prefix("file://") {
        let bytes =
            std::fs::read(local).with_context(|| format!("test file-url copy from {url}"))?;
        std::fs::write(dest, &bytes)?;
        on_progress(bytes.len() as u64, bytes.len() as u64);
        return Ok(());
    }
    bail!(
        "download_to: only file:// URLs are accepted at runtime (got '{url}'); \
         use 'mur agent voice download' from the CLI to fetch model weights"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    fn sha256_hex(content: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(content);
        format!("{:x}", h.finalize())
    }

    fn make_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content).unwrap();
        p
    }

    /// A named temp file holding `content`, written through the handle
    /// `tempfile` already owns.
    ///
    /// `fs::write(src.path(), …)` opens a SECOND write handle to a path the
    /// `NamedTempFile` still holds open. POSIX allows that; Windows
    /// intermittently refuses it with `Access is denied` (os error 5), which
    /// reddened a CI run on a PR that touched none of this code and passed on
    /// a bare re-run — the worst shape of flake, since it reads as "your change
    /// broke Windows".
    ///
    /// The file lives only as long as the returned handle, so callers must bind
    /// it for the duration of the test rather than drop it on the spot.
    fn temp_file_with(content: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content).unwrap();
        f
    }

    #[test]
    fn already_present_matching_hash_skips_download() {
        let tmp = tempfile::tempdir().unwrap();
        let content = b"pretend model bytes";
        // Box::leak is required: ModelSpec.sha256 is &'static str, but the hash
        // is computed at runtime in this test — leak produces a 'static reference.
        let expected_sha: &'static str = Box::leak(sha256_hex(content).into_boxed_str());
        let target_dir = tmp.path().join("models/kokoro");
        std::fs::create_dir_all(&target_dir).unwrap();
        make_file(&target_dir, "model.onnx", content);

        let spec = ModelSpec {
            name: "model.onnx",
            url: "https://should-not-be-called",
            sha256: expected_sha,
            subdir: "kokoro",
        };

        let path = ensure_model(tmp.path(), &spec, |_, _| {}).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn file_url_copies_content() {
        let content = b"kokoro model data";
        let src = temp_file_with(content);
        // Box::leak is required: ModelSpec.sha256 is &'static str, but the hash
        // is computed at runtime in this test — leak produces a 'static reference.
        let expected_sha: &'static str = Box::leak(sha256_hex(content).into_boxed_str());

        let mur_tmp = tempfile::tempdir().unwrap();
        let url: &'static str =
            Box::leak(format!("file://{}", src.path().display()).into_boxed_str());

        let spec = ModelSpec {
            name: "kokoro-test.onnx",
            url,
            sha256: expected_sha,
            subdir: "kokoro",
        };

        let path = ensure_model(mur_tmp.path(), &spec, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), content);
    }

    #[test]
    fn hash_mismatch_returns_error() {
        let src = temp_file_with(b"real content");
        let url: &'static str =
            Box::leak(format!("file://{}", src.path().display()).into_boxed_str());

        let mur_tmp = tempfile::tempdir().unwrap();
        let spec = ModelSpec {
            name: "bad.onnx",
            url,
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            subdir: "kokoro",
        };
        let result = ensure_model(mur_tmp.path(), &spec, |_, _| {});
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("sha256 mismatch"),
            "expected sha256 mismatch in: {msg}"
        );
    }

    #[test]
    fn https_url_rejected_at_runtime() {
        let mur_tmp = tempfile::tempdir().unwrap();
        let spec = ModelSpec {
            name: "model.bin",
            url: "https://example.com/model.bin",
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            subdir: "whisper",
        };
        let result = ensure_model(mur_tmp.path(), &spec, |_, _| {});
        assert!(result.is_err());
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("file://"),
            "expected file:// guard error, got: {msg}"
        );
    }
}
