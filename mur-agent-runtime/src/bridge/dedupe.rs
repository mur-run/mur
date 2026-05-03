use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const TREE_NAME: &str = "seen";
/// Sweep TTL-expired keys every Nth lookup. 256 = busy bridges sweep ~once
/// per few hundred messages; idle bridges almost never.
const SWEEP_EVERY: u32 = 256;

#[derive(thiserror::Error, Debug)]
pub enum DedupeError {
    #[error("sled error: {0}")]
    Sled(#[from] sled::Error),
    #[error("system time: {0}")]
    Time(#[from] std::time::SystemTimeError),
}

pub struct DedupeStore {
    _db: sled::Db,
    tree: sled::Tree,
    bridge_id: String,
    counter: std::sync::atomic::AtomicU32,
}

impl DedupeStore {
    pub fn open(dir: &Path, bridge_id: impl Into<String>) -> Result<Self, DedupeError> {
        let db = sled::open(dir.join("seen.sled"))?;
        let tree = db.open_tree(TREE_NAME)?;
        Ok(Self {
            _db: db,
            tree,
            bridge_id: bridge_id.into(),
            counter: 0.into(),
        })
    }

    fn make_key(&self, msg_id: &str) -> Vec<u8> {
        let mut k = Vec::with_capacity(self.bridge_id.len() + 1 + msg_id.len());
        k.extend_from_slice(self.bridge_id.as_bytes());
        k.push(0);
        k.extend_from_slice(msg_id.as_bytes());
        k
    }

    pub fn mark_seen(&mut self, msg_id: &str) -> Result<(), DedupeError> {
        let key = self.make_key(msg_id);
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        self.tree.insert(&key, &now.to_le_bytes())?;
        Ok(())
    }

    pub fn is_seen(&self, msg_id: &str) -> Result<bool, DedupeError> {
        let key = self.make_key(msg_id);
        let hit = self.tree.get(&key)?.is_some();
        let n = self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n.wrapping_add(1) % SWEEP_EVERY == 0 {
            let _ = self.sweep_expired();
        }
        Ok(hit)
    }

    pub(crate) fn sweep_expired(&self) -> Result<usize, DedupeError> {
        Ok(0) // implemented in M-c1.2.3
    }
}
