//! The final `Task.usage` of recent turns, keyed by task id (#2083).
//!
//! A caller normally reads a turn's usage off the `Task` the turn returns. A
//! fleet's delegate dial can lose that reply — an io timeout on a member that
//! went quiet, a reply cut short — while the member's turn still ran and was
//! billed. `tasks/get` hands the usage back from here, so the fleet cost
//! guard can account a turn whose reply never arrived instead of counting it
//! as free.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use serde_json::Value;

/// How many finished turns' usage is kept. Matches the task-state registry's
/// cap, so a task `tasks/get` can still name usually still has its usage.
const MAX_ENTRIES: usize = super::MAX_REGISTRY_ENTRIES;

#[derive(Debug, Default)]
pub(crate) struct TaskUsageStore {
    inner: Mutex<Store>,
}

#[derive(Debug, Default)]
struct Store {
    by_id: HashMap<String, Value>,
    /// Insertion order, oldest first, for eviction.
    order: VecDeque<String>,
}

impl TaskUsageStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record (or replace) the final usage of `id`'s turn.
    pub(crate) fn insert(&self, id: &str, usage: Value) {
        let mut s = self.lock();
        if s.by_id.insert(id.to_string(), usage).is_none() {
            s.order.push_back(id.to_string());
            while s.order.len() > MAX_ENTRIES {
                if let Some(oldest) = s.order.pop_front() {
                    s.by_id.remove(&oldest);
                }
            }
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<Value> {
        self.lock().by_id.get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_latest_usage_and_evicts_the_oldest_past_the_cap() {
        let s = TaskUsageStore::default();
        s.insert("t0", serde_json::json!({"input_tokens": 1}));
        s.insert("t0", serde_json::json!({"input_tokens": 2}));
        assert_eq!(
            s.get("t0").unwrap()["input_tokens"],
            2,
            "replaced, not duplicated"
        );
        for i in 1..=MAX_ENTRIES {
            s.insert(&format!("t{i}"), serde_json::json!({}));
        }
        assert!(s.get("t0").is_none(), "oldest evicted past the cap");
        assert!(s.get(&format!("t{MAX_ENTRIES}")).is_some());
        assert_eq!(s.lock().order.len(), MAX_ENTRIES);
    }
}
