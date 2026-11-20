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

/// Same bound as the task registry. Only settled claims are ever evicted: a
/// claim whose submit call is still running (the turn itself, since
/// `message/send` returns when the turn ends) is pinned, so a flood of
/// submissions cannot push out the claim of a live turn.
const MAX_ENTRIES: usize = 1_024;

#[derive(Default)]
pub struct TaskSubmitters {
    inner: Mutex<Inner>,
}

struct Entry {
    /// `None`: claimed by a caller that is not a peer agent.
    agent: Option<String>,
    /// The submit call that made this claim has not returned yet.
    in_flight: bool,
}

#[derive(Default)]
struct Inner {
    by_task: HashMap<String, Entry>,
    order: VecDeque<String>,
}

/// Outcome of [`TaskSubmitters::claim`].
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// This call now holds the claim; settle it when the call returns.
    New,
    /// Someone claimed the id first; this call holds nothing.
    Taken,
    /// Every slot belongs to a running submission; nothing was claimed.
    Full,
}

impl TaskSubmitters {
    /// Claim `task_id` for `agent` (`None` when the caller is not a peer
    /// agent), pinned until [`settle`](Self::settle). The first claim wins:
    /// a later caller reusing the id cannot take over someone else's task.
    pub fn claim(&self, task_id: &str, agent: Option<&str>) -> Claim {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.by_task.contains_key(task_id) {
            return Claim::Taken;
        }
        if g.by_task.len() >= MAX_ENTRIES && !g.evict_one_settled() {
            return Claim::Full;
        }
        g.order.push_back(task_id.to_string());
        g.by_task.insert(
            task_id.to_string(),
            Entry {
                agent: agent.map(str::to_string),
                in_flight: true,
            },
        );
        Claim::New
    }

    /// The submit call holding `task_id`'s claim returned. A refused call
    /// (`accepted == false`) releases the id; an accepted one keeps it,
    /// now evictable, so the submitter can still follow the finished task.
    pub fn settle(&self, task_id: &str, accepted: bool) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if accepted {
            if let Some(e) = g.by_task.get_mut(task_id) {
                e.in_flight = false;
            }
        } else if g.by_task.remove(task_id).is_some() {
            g.order.retain(|id| id != task_id);
        }
    }

    pub fn is_submitter(&self, task_id: &str, agent: &str) -> bool {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.by_task
            .get(task_id)
            .is_some_and(|e| e.agent.as_deref() == Some(agent))
    }
}

impl Inner {
    /// Drop the oldest settled claim; false when every claim is in flight.
    fn evict_one_settled(&mut self) -> bool {
        let Some(pos) = self
            .order
            .iter()
            .position(|id| self.by_task.get(id).is_some_and(|e| !e.in_flight))
        else {
            return false;
        };
        if let Some(id) = self.order.remove(pos) {
            self.by_task.remove(&id);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled(s: &TaskSubmitters, id: &str, agent: Option<&str>) {
        assert_eq!(s.claim(id, agent), Claim::New);
        s.settle(id, true);
    }

    #[test]
    fn the_first_claim_wins() {
        let s = TaskSubmitters::default();
        assert_eq!(s.claim("ct-1", Some("mur")), Claim::New);
        assert_eq!(s.claim("ct-1", Some("bob")), Claim::Taken);
        assert!(s.is_submitter("ct-1", "mur"));
        assert!(!s.is_submitter("ct-1", "bob"));
        assert!(!s.is_submitter("ct-2", "mur"));
    }

    #[test]
    fn the_oldest_settled_entry_is_evicted_at_the_bound() {
        let s = TaskSubmitters::default();
        for i in 0..=MAX_ENTRIES {
            settled(&s, &format!("ct-{i}"), Some("mur"));
        }
        assert!(!s.is_submitter("ct-0", "mur"));
        assert!(s.is_submitter(&format!("ct-{MAX_ENTRIES}"), "mur"));
    }

    /// #2491 review: a flood of submissions must not push out the claim of a
    /// turn that is still running, or a sibling could re-claim and cancel it.
    #[test]
    fn a_running_claim_is_never_evicted() {
        let s = TaskSubmitters::default();
        assert_eq!(s.claim("live", Some("mur")), Claim::New);
        for i in 0..2 * MAX_ENTRIES {
            let id = format!("flood-{i}");
            if s.claim(&id, Some("bob")) == Claim::New {
                s.settle(&id, true);
            }
        }
        assert!(s.is_submitter("live", "mur"));
        assert_eq!(s.claim("live", Some("bob")), Claim::Taken);
    }

    #[test]
    fn a_full_table_of_running_claims_refuses_a_new_one() {
        let s = TaskSubmitters::default();
        for i in 0..MAX_ENTRIES {
            assert_eq!(s.claim(&format!("ct-{i}"), Some("mur")), Claim::New);
        }
        assert_eq!(s.claim("one-more", Some("mur")), Claim::Full);
        s.settle("ct-0", true);
        assert_eq!(s.claim("one-more", Some("mur")), Claim::New);
        assert!(!s.is_submitter("ct-0", "mur"));
    }

    #[test]
    fn a_refused_call_releases_its_claim() {
        let s = TaskSubmitters::default();
        assert_eq!(s.claim("ct-1", Some("bob")), Claim::New);
        s.settle("ct-1", false);
        assert!(!s.is_submitter("ct-1", "bob"));
        assert_eq!(s.claim("ct-1", Some("mur")), Claim::New);
    }

    #[test]
    fn a_user_claim_blocks_a_later_agent_claim() {
        let s = TaskSubmitters::default();
        settled(&s, "task-1", None);
        assert_eq!(s.claim("task-1", Some("bob")), Claim::Taken);
        assert!(!s.is_submitter("task-1", "bob"));
    }
}
