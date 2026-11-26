//! #2581 part 2: refusals that outlive the turn they happened in.
//!
//! A refused call ends an ordinary turn (`hitl_approval_required` /
//! `hitl_denied`), and a delegated turn withdraws every exec tool (#2587), so
//! the retry that matters comes in the NEXT turn of the same conversation.
//! A conversation is the chain of turns linked by `context.task_id` (each
//! turn names the one it continues; a failed turn is threaded too, #2071).
//!
//! Each turn holds the `action_hash`es of the `Destructive` calls refused in
//! it or inherited from its parent. A turn with no parent — a new
//! conversation, or a one-shot `mur agent send` — starts empty, so another
//! conversation never inherits. A record is dropped once the decision store
//! holds an allow for it (see `tools/guarded/denial_escalation.rs`).
//!
//! Records stay after their turn ends: the next turn has to find them. The
//! ledger keeps at most [`MAX_TURNS`] turns' records, oldest first out.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

/// Turns whose records are kept. A conversation only ever reads its direct
/// parent, so this bounds memory without forgetting anything a live
/// conversation can still reach.
pub(crate) const MAX_TURNS: usize = 1024;

#[derive(Default)]
struct Inner {
    /// turn task id → refused `action_hash`es still open in that turn.
    open: HashMap<String, Vec<String>>,
    /// Insertion order of `open`'s keys, for eviction.
    order: VecDeque<String>,
}

/// Shared between the runner (turn start) and the gate (arm / read / clear).
#[derive(Clone, Default)]
pub(crate) struct DenialLedger(Arc<Mutex<Inner>>);

impl DenialLedger {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Turn `turn` starts: it carries `parent`'s open records, or none.
    pub(crate) fn begin_turn(&self, turn: &str, parent: Option<&str>) {
        let mut g = self.lock();
        let inherited = parent.and_then(|p| g.open.get(p).cloned());
        match inherited {
            Some(hashes) if !hashes.is_empty() => put(&mut g, turn, hashes),
            _ => {
                g.open.remove(turn);
            }
        }
    }

    /// Record a refused `Destructive` call in `turn`.
    pub(crate) fn record(&self, turn: &str, action_hash: String) {
        let mut g = self.lock();
        let mut hashes = g.open.get(turn).cloned().unwrap_or_default();
        if !hashes.contains(&action_hash) {
            hashes.push(action_hash);
        }
        put(&mut g, turn, hashes);
    }

    /// The records open in `turn`.
    pub(crate) fn open(&self, turn: &str) -> Vec<String> {
        self.lock().open.get(turn).cloned().unwrap_or_default()
    }

    /// Close `cleared` in `turn` (the human approved them). Later turns of
    /// this conversation inherit what is left.
    pub(crate) fn clear(&self, turn: &str, cleared: &[String]) {
        let mut g = self.lock();
        if let Some(hashes) = g.open.get_mut(turn) {
            hashes.retain(|h| !cleared.contains(h));
        }
    }
}

fn put(g: &mut Inner, turn: &str, hashes: Vec<String>) {
    if g.open.insert(turn.to_string(), hashes).is_none() {
        g.order.push_back(turn.to_string());
    }
    while g.order.len() > MAX_TURNS {
        if let Some(old) = g.order.pop_front() {
            g.open.remove(&old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_inherits_and_a_stranger_does_not() {
        let l = DenialLedger::default();
        l.record("t1", "h".into());
        l.begin_turn("t2", Some("t1"));
        l.begin_turn("x1", None);
        l.begin_turn("y1", Some("unknown"));
        assert_eq!(l.open("t2"), vec!["h".to_string()]);
        assert!(l.open("x1").is_empty());
        assert!(l.open("y1").is_empty());
    }

    #[test]
    fn a_cleared_record_is_not_inherited() {
        let l = DenialLedger::default();
        l.record("t1", "h".into());
        l.begin_turn("t2", Some("t1"));
        l.clear("t2", &["h".into()]);
        l.begin_turn("t3", Some("t2"));
        assert!(l.open("t3").is_empty());
    }

    #[test]
    fn the_ledger_is_bounded() {
        let l = DenialLedger::default();
        for i in 0..=MAX_TURNS {
            l.record(&format!("t{i}"), "h".into());
        }
        assert!(l.open("t0").is_empty(), "oldest turn evicted");
        assert_eq!(l.open(&format!("t{MAX_TURNS}")).len(), 1);
    }
}
