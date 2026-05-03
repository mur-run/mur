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
}
