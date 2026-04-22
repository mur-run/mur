//! File-per-key filesystem cache for Phase 3.5 abstractive hit summaries.
//!
//! Pure I/O, no LLM knowledge. Values are UTF-8 text. Keys are 64-char lowercase
//! hex (sha256). Layout: `~/.mur/conversations/cache/abstractive/<key>.txt`.
//! Writes use temp + rename for atomicity (matches `store/yaml.rs`).
#![allow(dead_code)] // wired by Task 5 (abstractive::compress_hit).

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Root for all abstractive-summary cache files.
pub fn cache_dir(root_override: Option<&str>) -> PathBuf {
    super::super::paths::conversations_root(root_override)
        .join("cache")
        .join("abstractive")
}

/// Deterministic cache key: `sha256("mur-abstract-v1" || "|" || model || "|" ||
/// target_tokens || "|" || content)` → 64-char lowercase hex.
/// Bump the version prefix literal (`"mur-abstract-v1"`) whenever the prompt
/// template or validator semantics change, so old cache entries naturally
/// become misses rather than requiring a sweep.
pub fn cache_key(model: &str, target_tokens: usize, content: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"mur-abstract-v1|");
    h.update(model.as_bytes());
    h.update(b"|");
    h.update(target_tokens.to_le_bytes());
    h.update(b"|");
    h.update(content.as_bytes());
    hex::encode(h.finalize())
}

/// Read a value by key. Any filesystem error (missing file, permission denied,
/// I/O error) returns `None` — misses are the common case and must never
/// surface as errors to the overflow cascade.
pub fn cache_get(key: &str, root_override: Option<&str>) -> Option<String> {
    let path = cache_dir(root_override).join(format!("{key}.txt"));
    match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::debug!(?path, err = ?e, "cache miss (read error)");
            None
        }
    }
}

/// Write a value under a key. Uses temp-file + rename for atomicity.
/// Creates `cache_dir()` on first call.
pub fn cache_put(key: &str, value: &str, root_override: Option<&str>) -> Result<()> {
    let dir = cache_dir(root_override);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {dir:?}"))?;
    let final_path = dir.join(format!("{key}.txt"));
    let tmp_path = dir.join(format!("{key}.txt.tmp"));
    std::fs::write(&tmp_path, value).with_context(|| format!("write {tmp_path:?}"))?;
    std::fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {tmp_path:?} → {final_path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_tmp<R>(f: impl FnOnce(&str) -> R) -> R {
        let tmp = tempfile::tempdir().unwrap();
        f(tmp.path().to_str().unwrap())
    }

    #[test]
    fn cache_key_is_stable() {
        let a = cache_key("qwen3:14b", 128, "hello world");
        let b = cache_key("qwen3:14b", 128, "hello world");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    #[test]
    fn cache_key_differs_by_model() {
        let a = cache_key("qwen3:14b", 128, "hello");
        let b = cache_key("qwen3:4b", 128, "hello");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_differs_by_target_tokens() {
        let a = cache_key("m", 128, "hello");
        let b = cache_key("m", 256, "hello");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_differs_by_content() {
        let a = cache_key("m", 128, "hello");
        let b = cache_key("m", 128, "world");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_put_then_get_roundtrip() {
        with_tmp(|root| {
            let key = cache_key("m", 64, "content A");
            cache_put(&key, "summary of A", Some(root)).unwrap();
            assert_eq!(cache_get(&key, Some(root)).as_deref(), Some("summary of A"));
        });
    }

    #[test]
    fn cache_get_missing_returns_none() {
        with_tmp(|root| {
            let key = cache_key("m", 64, "never written");
            assert!(cache_get(&key, Some(root)).is_none());
        });
    }

    #[test]
    fn cache_put_is_atomic_no_tmp_left_behind() {
        with_tmp(|root| {
            let key = cache_key("m", 64, "content");
            cache_put(&key, "val", Some(root)).unwrap();
            let dir = cache_dir(Some(root));
            let tmp_path = dir.join(format!("{key}.txt.tmp"));
            assert!(!tmp_path.exists(), "temp file must be renamed away");
            let final_path = dir.join(format!("{key}.txt"));
            assert!(final_path.exists());
        });
    }

    #[test]
    fn cache_put_creates_dir_on_first_call() {
        with_tmp(|root| {
            let dir = cache_dir(Some(root));
            assert!(!dir.exists(), "precondition: dir missing");
            let key = cache_key("m", 64, "x");
            cache_put(&key, "y", Some(root)).unwrap();
            assert!(dir.exists(), "cache_put should create the dir");
        });
    }

    #[test]
    fn cache_key_version_prefix_changes_key() {
        // The hardcoded "mur-abstract-v1" prefix must be part of every cache key —
        // that's the invalidation story when we bump to v2.
        // Recompute the hash with NO prefix and assert it differs.
        let actual = cache_key("m", 128, "hello");
        let without_prefix = {
            let mut h = Sha256::new();
            h.update(b"m");
            h.update(b"|");
            h.update(128usize.to_le_bytes());
            h.update(b"|");
            h.update(b"hello");
            hex::encode(h.finalize())
        };
        assert_ne!(
            actual, without_prefix,
            "version prefix MUST be part of cache_key output"
        );
    }
}
