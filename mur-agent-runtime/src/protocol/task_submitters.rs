//! Who submitted each caller-named task, so the submitter can follow and stop
//! it without the owner's proof (#2488).
//!
//! A fleet launched by a sealed agent's `fleet_run` runs inside that agent's
//! sandbox and cannot read the home approval token, so it has no owner proof.
//! It must still be able to `tasks/get` and `tasks/cancel` the member turn it
//! started; the kill-switch and late usage accounting depend on it. Anyone
//! else — a sibling that merely passed `accepts_from` — still needs the proof.
//!
//! Every caller-named submission claims its id, but only a peer agent
//! ([`Caller::Agent`]) is ever recognised as a submitter. A non-agent lineage
//! ("the user") claims without being recognised: lineage cannot tell the
//! user's CLI from any other process outside an agent, and the user has the
//! proof anyway. The claim still matters — it stops a sibling from naming
//! the same id later and adopting a turn it did not start.
//!
//! [`Caller::Agent`]: crate::communication_policy::Caller::Agent

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// Methods that start a turn under a caller-supplied `task_id`.
pub const SUBMIT_METHODS: &[&str] = &["message/send", "channel/delegate"];

/// Owner methods a task's submitter may call on that task.
pub const SUBMITTER_METHODS: &[&str] = &["tasks/get", "tasks/cancel"];

/// Same bound as the task registry, so an entry outlives its task state.
const MAX_ENTRIES: usize = 1_024;

#[derive(Default)]
pub struct TaskSubmitters {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// `None`: claimed by a caller that is not a peer agent.
    by_task: HashMap<String, Option<String>>,
    order: VecDeque<String>,
}

impl TaskSubmitters {
    /// Claim `task_id` for `agent` (`None` when the caller is not a peer
    /// agent). The first claim wins: a later caller reusing the id cannot
    /// take over someone else's task.
    pub fn record(&self, task_id: &str, agent: Option<&str>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.by_task.contains_key(task_id) {
            return;
        }
        while g.by_task.len() >= MAX_ENTRIES {
            let Some(oldest) = g.order.pop_front() else {
                break;
            };
            g.by_task.remove(&oldest);
        }
        g.order.push_back(task_id.to_string());
        g.by_task
            .insert(task_id.to_string(), agent.map(str::to_string));
    }

    pub fn is_submitter(&self, task_id: &str, agent: &str) -> bool {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.by_task
            .get(task_id)
            .is_some_and(|a| a.as_deref() == Some(agent))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_claim_wins() {
        let s = TaskSubmitters::default();
        s.record("ct-1", Some("mur"));
        s.record("ct-1", Some("bob"));
        assert!(s.is_submitter("ct-1", "mur"));
        assert!(!s.is_submitter("ct-1", "bob"));
        assert!(!s.is_submitter("ct-2", "mur"));
    }

    #[test]
    fn the_oldest_entry_is_evicted_at_the_bound() {
        let s = TaskSubmitters::default();
        for i in 0..=MAX_ENTRIES {
            s.record(&format!("ct-{i}"), Some("mur"));
        }
        assert!(!s.is_submitter("ct-0", "mur"));
        assert!(s.is_submitter(&format!("ct-{MAX_ENTRIES}"), "mur"));
    }

    #[test]
    fn a_user_claim_blocks_a_later_agent_claim() {
        let s = TaskSubmitters::default();
        s.record("task-1", None);
        s.record("task-1", Some("bob"));
        assert!(!s.is_submitter("task-1", "bob"));
    }
}
