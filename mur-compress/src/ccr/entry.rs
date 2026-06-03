//! A stored original, retrievable by hash, with parsed items for query filter.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressedEntry {
    pub hash: String,
    pub content_type: String,
    pub created_at: u64,
    pub ttl_secs: u64,
    pub original_text: String,
    pub items: Vec<String>,
    pub original_tokens: usize,
    pub item_count: usize,
}

impl CompressedEntry {
    pub fn is_expired(&self, now: u64) -> bool {
        self.ttl_secs > 0 && now > self.created_at + self.ttl_secs
    }
}
