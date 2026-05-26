//! Content-hash cache for maintenance LLM calls.
//! Keyed by (model, prompt, budget) → SHA-256 hex.
//! TTL-pruned on read; atomic write via temp + rename.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

use super::TokenBudget;

/// Build a content-hash cache key from the call parameters.
pub fn key(model: &str, prompt: &str, budget: TokenBudget) -> String {
    let mut h = Sha256::new();
    h.update(model.as_bytes());
    h.update(b"\x00");
    h.update(prompt.as_bytes());
    h.update(b"\x00");
    h.update(budget.max_input.to_le_bytes());
    h.update(budget.max_output.to_le_bytes());
    format!("{:x}", h.finalize())
}

/// Load a cached response if it exists and is within TTL.
pub fn load(key: &str, ttl: Duration) -> anyhow::Result<Option<String>> {
    let path = cache_path(key);
    if !path.exists() {
        return Ok(None);
    }
    let meta = std::fs::metadata(&path)?;
    let mtime = DateTime::<Utc>::from(meta.modified()?);
    if Utc::now() - mtime > ttl {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(&path)?))
}

/// Save a response to the cache with atomic write.
pub fn save(key: &str, body: &str) -> anyhow::Result<()> {
    let path = cache_path(key);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn cache_path(key: &str) -> PathBuf {
    cache_path_at(&mur_home(), key)
}

fn cache_path_at(home: &PathBuf, key: &str) -> PathBuf {
    home.join("skill_llm_cache")
        .join(&key[..2.min(key.len())])
        .join(format!("{}.json", &key[2.min(key.len())..]))
}

pub(crate) fn mur_home() -> PathBuf {
    if let Ok(p) = std::env::var("MUR_HOME") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};
    use tempfile::TempDir;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn key1() -> String {
        key("test-model", "hello", TokenBudget::DEFAULT)
    }

    fn key2() -> String {
        key("test-model", "world", TokenBudget::DEFAULT)
    }

    fn with_test_home<T>(f: impl FnOnce() -> T) -> T {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        unsafe { std::env::set_var("MUR_HOME", dir.path().as_os_str()) };
        f()
    }

    #[test]
    fn cache_save_and_load() {
        with_test_home(|| {
            let k = key1();
            save(&k, "response body").unwrap();
            let loaded = load(&k, Duration::days(30)).unwrap();
            assert_eq!(loaded, Some("response body".to_string()));
        });
    }

    #[test]
    fn cache_different_keys_produce_different_entries() {
        let a = key1();
        let b = key2();
        assert_ne!(a, b);
    }

    #[test]
    fn cache_ttl_expired() {
        with_test_home(|| {
            let k = key1();
            save(&k, "stale").unwrap();
            let loaded = load(&k, Duration::seconds(-1)).unwrap();
            assert!(loaded.is_none(), "expired TTL should return None");
        });
    }
}
