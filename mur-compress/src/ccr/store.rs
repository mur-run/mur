//! Persistent, bounded CCR store. One JSON(.gz) file per entry under
//! <dir>/entries/. Atomic temp+rename writes. TTL on read; size/count eviction
//! is LRU — `get()` touches an entry's mtime so frequently-retrieved originals
//! outlive cold ones.

use std::io::{Read, Write};
use std::path::PathBuf;

use crate::ccr::entry::CompressedEntry;
use crate::tokenizer::TokenCounter;
use crate::types::ContentType;

pub fn hash_content(s: &str) -> String {
    let hex = blake3::hash(s.as_bytes()).to_hex();
    hex[..24].to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data)?;
    enc.finish()
}

fn gunzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut dec = flate2::read::GzDecoder::new(data);
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

pub struct CcrStore {
    entries_dir: PathBuf,
    ttl_secs: u64,
    max_entries: usize,
    max_bytes: u64,
    compress_at_rest: bool,
}

impl CcrStore {
    pub fn new(
        dir: impl Into<PathBuf>,
        ttl_secs: u64,
        max_entries: usize,
        max_bytes: u64,
        compress_at_rest: bool,
    ) -> std::io::Result<Self> {
        let entries_dir = dir.into().join("entries");
        std::fs::create_dir_all(&entries_dir)?;
        Ok(Self {
            entries_dir,
            ttl_secs,
            max_entries,
            max_bytes,
            compress_at_rest,
        })
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl_secs
    }

    fn path_for(&self, hash: &str) -> PathBuf {
        let ext = if self.compress_at_rest {
            "json.gz"
        } else {
            "json"
        };
        self.entries_dir.join(format!("{hash}.{ext}"))
    }

    pub fn put(&self, entry: &CompressedEntry) -> std::io::Result<()> {
        let path = self.path_for(&entry.hash);
        let json = serde_json::to_vec(entry)?;
        let bytes = if self.compress_at_rest {
            gzip(&json)?
        } else {
            json
        };
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        self.evict_if_needed()?;
        Ok(())
    }

    /// Build + store an entry for `original`, returning its hash.
    pub fn put_original(
        &self,
        original: &str,
        items: Vec<String>,
        content_type: ContentType,
        tok: &dyn TokenCounter,
    ) -> std::io::Result<String> {
        let hash = hash_content(original);
        let item_count = items.len();
        let entry = CompressedEntry {
            hash: hash.clone(),
            content_type: content_type.as_str().to_string(),
            created_at: now_secs(),
            ttl_secs: self.ttl_secs,
            original_text: original.to_string(),
            items,
            original_tokens: tok.count(original),
            item_count,
        };
        self.put(&entry)?;
        Ok(hash)
    }

    pub fn get(&self, hash: &str) -> std::io::Result<Option<CompressedEntry>> {
        let path = self.path_for(hash);
        let raw = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let json = if self.compress_at_rest {
            gunzip(&raw)?
        } else {
            raw
        };
        let entry: CompressedEntry = serde_json::from_slice(&json)?;
        if entry.is_expired(now_secs()) {
            let _ = std::fs::remove_file(&path);
            return Ok(None);
        }
        // Touch mtime so eviction (which orders by mtime) is LRU, not FIFO.
        // Best-effort: a failure here only affects eviction ordering.
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        Ok(Some(entry))
    }

    pub fn stats(&self) -> (usize, u64) {
        let mut count = 0usize;
        let mut bytes = 0u64;
        if let Ok(rd) = std::fs::read_dir(&self.entries_dir) {
            for e in rd.flatten() {
                if let Ok(md) = e.metadata()
                    && md.is_file()
                    && e.path().extension().is_some_and(|x| x != "tmp")
                {
                    count += 1;
                    bytes += md.len();
                }
            }
        }
        (count, bytes)
    }

    fn evict_if_needed(&self) -> std::io::Result<()> {
        let mut files: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
        let mut total = 0u64;
        for e in std::fs::read_dir(&self.entries_dir)? {
            let e = e?;
            let md = e.metadata()?;
            // Skip non-files and in-flight temp files: a `.tmp` belongs to an
            // active writer (possibly another process) and must not be counted
            // toward the budget or deleted out from under its rename.
            if !md.is_file() || e.path().extension().is_some_and(|x| x == "tmp") {
                continue;
            }
            let mtime = md.modified().unwrap_or(std::time::UNIX_EPOCH);
            total += md.len();
            files.push((e.path(), mtime, md.len()));
        }
        files.sort_by_key(|(_, t, _)| *t); // oldest first
        let mut idx = 0;
        while idx < files.len()
            && ((files.len() - idx) > self.max_entries || total > self.max_bytes)
        {
            let (p, _, sz) = &files[idx];
            let _ = std::fs::remove_file(p);
            total = total.saturating_sub(*sz);
            idx += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::HeuristicCounter;
    use std::path::Path;

    fn store(dir: &Path, compress: bool) -> CcrStore {
        CcrStore::new(dir, 3600, 100, 1 << 30, compress).unwrap()
    }

    #[test]
    fn put_get_roundtrip_plain_and_gz() {
        for compress in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let s = store(dir.path(), compress);
            let h = s
                .put_original(
                    "a\nb\nc",
                    vec!["a".into(), "b".into(), "c".into()],
                    ContentType::SearchResults,
                    &HeuristicCounter,
                )
                .unwrap();
            let got = s.get(&h).unwrap().expect("entry exists");
            assert_eq!(got.original_text, "a\nb\nc");
            assert_eq!(got.item_count, 3);
        }
    }

    #[test]
    fn expired_entry_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        // ttl_secs = 0 means "no expiry"; use 1 and a backdated entry.
        let s = CcrStore::new(dir.path(), 1, 100, 1 << 30, false).unwrap();
        let mut entry = CompressedEntry {
            hash: hash_content("x"),
            content_type: "generic".into(),
            created_at: 0, // far in the past
            ttl_secs: 1,
            original_text: "x".into(),
            items: vec![],
            original_tokens: 1,
            item_count: 0,
        };
        entry.hash = hash_content("x");
        s.put(&entry).unwrap();
        assert!(s.get(&entry.hash).unwrap().is_none());
    }

    #[test]
    fn missing_hash_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), false);
        assert!(s.get("deadbeef").unwrap().is_none())
    }
}
