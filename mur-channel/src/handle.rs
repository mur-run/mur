//! The short, typeable handle shown for a channel whose id is a UUID: the
//! shortest prefix of the id that no other channel on this machine shares,
//! and never shorter than [`MIN_HANDLE_LEN`].
//!
//! A fixed-length prefix is not enough. Channel ids are UUIDv7, whose first
//! twelve hex characters are the creation time in milliseconds, so every
//! channel created within the same ~65 s shares its first eight (#2538).
//!
//! The prefix is taken over the id with its dashes removed, so a handle is
//! hex only and can be typed as it stands; a resolver must match it against
//! the id the same way ([`handle_key`]). Uniqueness is computed over every
//! channel in the index, so a handle is also unique within any subset of
//! them that a resolver happens to search.

use std::collections::HashMap;

/// Shortest handle ever shown. Eight is what a handle always was, so a
/// channel with no near neighbour keeps the handle it had before.
pub const MIN_HANDLE_LEN: usize = 8;

/// The form of `id` that handles are prefixes of: lowercase, dashes removed.
/// `None` for an id that is not a hex UUID-like string (a run's
/// `fleet-<name>`), which has no short handle and is named whole.
pub fn handle_key(id: &str) -> Option<String> {
    let key: String = id
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let hex = !key.is_empty() && key.chars().all(|c| c.is_ascii_hexdigit());
    hex.then_some(key)
}

/// The handle length of every hex id in a set, computed once per listing.
#[derive(Debug, Default, Clone)]
pub struct Handles {
    lens: HashMap<String, usize>,
}

impl Handles {
    /// Handles for `ids`. Non-hex ids are ignored: they never collide with a
    /// hex prefix and are shown whole.
    pub fn from_ids<I, S>(ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut keys: Vec<String> = ids
            .into_iter()
            .filter_map(|id| handle_key(id.as_ref()))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        let lcp = |a: &str, b: &str| a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
        let mut lens = HashMap::with_capacity(keys.len());
        for (i, k) in keys.iter().enumerate() {
            // Sorted, so the longest prefix `k` shares with any other key is
            // the one it shares with a neighbour.
            let prev = i.checked_sub(1).map_or(0, |p| lcp(&keys[p], k));
            let next = keys.get(i + 1).map_or(0, |n| lcp(k, n));
            let len = (prev.max(next) + 1).max(MIN_HANDLE_LEN).min(k.len());
            lens.insert(k.clone(), len);
        }
        Self { lens }
    }

    /// The handle for `id`, or `None` when `id` is not hex (see
    /// [`handle_key`]). An id this set has never seen gets the minimum
    /// length — the best guess there is without its neighbours.
    pub fn of(&self, id: &str) -> Option<String> {
        let key = handle_key(id)?;
        let len = self
            .lens
            .get(&key)
            .copied()
            .unwrap_or(MIN_HANDLE_LEN)
            .min(key.len());
        Some(key[..len].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two ids minted in the same ~65 s window: same first eight hex.
    const A: &str = "01a16c01-c974-7ba0-985f-68a6b1028be2";
    const B: &str = "01a16c01-d001-7ba0-985f-68a6b1028be2";
    const C: &str = "019ed0af-5e38-7912-b554-dc335a8fc2db";

    #[test]
    fn a_lone_id_keeps_the_eight_character_handle() {
        let h = Handles::from_ids([A, C]);
        assert_eq!(h.of(C).unwrap(), "019ed0af");
        assert_eq!(h.of(A).unwrap(), "01a16c01");
    }

    #[test]
    fn ids_sharing_eight_characters_get_the_shortest_unique_prefix() {
        let h = Handles::from_ids([A, B, C]);
        assert_eq!(h.of(A).unwrap(), "01a16c01c");
        assert_eq!(h.of(B).unwrap(), "01a16c01d");
        assert_eq!(h.of(C).unwrap(), "019ed0af");
    }

    #[test]
    fn every_handle_is_a_prefix_of_exactly_one_key() {
        let ids: Vec<String> = (0..300u32)
            .map(|n| format!("01a16c01-{:04x}-7000-8000-{:012x}", n / 7, n))
            .collect();
        let h = Handles::from_ids(&ids);
        let keys: Vec<String> = ids.iter().map(|i| handle_key(i).unwrap()).collect();
        for id in &ids {
            let handle = h.of(id).unwrap();
            assert!(handle.len() >= MIN_HANDLE_LEN);
            let hits = keys.iter().filter(|k| k.starts_with(&handle)).count();
            assert_eq!(hits, 1, "{handle} for {id}");
        }
    }

    #[test]
    fn a_run_id_has_no_short_handle() {
        let h = Handles::from_ids(["fleet-dev", A]);
        assert_eq!(h.of("fleet-dev"), None);
        assert_eq!(handle_key("fleet-dev"), None);
        assert_eq!(handle_key("01A1-6C"), Some("01a16c".into()));
    }
}
