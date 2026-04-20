//! Content-addressed blob store at ~/.mur/conversations/blob/<sha256>.
//!
//! Used by `conversations::ingest::normalize` (Task 6) to stash large tool-result
//! bodies whose original isn't persisted elsewhere. `put()` is idempotent: the
//! same bytes always hash to the same filename and we skip the write if it already
//! exists.
#![allow(dead_code)] // Phase 1: stubs wired in by later tasks (migrate, retrieve).

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;

use super::paths::blob_root;

pub fn put(content: &[u8], root_override: Option<&str>) -> Result<String> {
    let sha = {
        let mut h = Sha256::new();
        h.update(content);
        hex::encode(h.finalize())
    };
    let root = blob_root(root_override);
    fs::create_dir_all(&root)?;
    let path = root.join(&sha);
    if !path.exists() {
        fs::write(&path, content).with_context(|| format!("writing blob {path:?}"))?;
    }
    Ok(sha)
}

pub fn get(sha: &str, root_override: Option<&str>) -> Result<Vec<u8>> {
    let path = blob_root(root_override).join(sha);
    fs::read(&path).with_context(|| format!("reading blob {path:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let sha = put(b"hello world", Some(root)).unwrap();
        assert_eq!(sha.len(), 64);
        assert_eq!(get(&sha, Some(root)).unwrap(), b"hello world");
    }

    #[test]
    fn put_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = put(b"same", Some(root)).unwrap();
        let b = put(b"same", Some(root)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn get_missing_errors() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(get("deadbeef", Some(tmp.path().to_str().unwrap())).is_err());
    }
}
