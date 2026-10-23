//! #2085 option C: approvals a headless caller chose to wait for.
//!
//! `mur agent send --wait-approval` cannot answer a prompt itself, but it can
//! wait while a human answers from another terminal. The turn opens a *window*
//! here; while it is open, the headless gate parks an undecided `Ask` call on
//! the ordinary `hitl_id → oneshot` (so `tool/hitl_respond` and its approval
//! authority are unchanged) and records it as *parked*, which is all
//! `hitl/pending` lists. Nothing here decides anything: an expired window, a
//! closed one, or a restarted runtime all end in the call not running.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;

/// One call waiting on a human. Carries the redacted label only — never the
/// tool input, same rule as the decision log (#1806).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ParkedCall {
    pub hitl_id: String,
    pub task_id: String,
    pub tool_name: String,
    pub label: String,
    pub parked_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Default)]
struct Inner {
    /// Turn id → until when an undecided call of that turn may wait.
    windows: HashMap<String, Instant>,
    /// `hitl_id` → the call waiting on it.
    calls: HashMap<String, ParkedCall>,
}

/// Shared by the runner (which parks) and the `hitl/pending` handler (which
/// lists). Cheap to clone. A std mutex: every critical section is a map
/// operation, never held across an await.
#[derive(Clone, Default)]
pub struct Parking(Arc<Mutex<Inner>>);

impl Parking {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned map is still a valid map; refusing to list or close a
        // window because another thread panicked would only strand a turn.
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Let this turn's undecided calls wait up to `wait` from now.
    pub fn open_window(&self, task_id: &str, wait: Duration) {
        self.lock()
            .windows
            .insert(task_id.to_string(), Instant::now() + wait);
    }

    /// Pull the window in to the turn's deadline when that comes first: the
    /// deadline is the outer bound, a parked call must not outlive it.
    pub fn clamp_window(&self, task_id: &str, deadline: Option<Instant>) {
        let Some(d) = deadline else { return };
        if let Some(until) = self.lock().windows.get_mut(task_id)
            && d < *until
        {
            *until = d;
        }
    }

    /// Time left in this turn's window. `None` = no window, or it has passed:
    /// the caller then refuses at once, exactly as without the flag.
    pub fn remaining(&self, task_id: &str) -> Option<Duration> {
        let until = *self.lock().windows.get(task_id)?;
        let left = until.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    /// The turn is over: no more parking for it, and nothing of it listed.
    pub fn close_window(&self, task_id: &str) {
        let mut g = self.lock();
        g.windows.remove(task_id);
        g.calls.retain(|_, c| c.task_id != task_id);
    }

    pub fn park(&self, call: ParkedCall) {
        self.lock().calls.insert(call.hitl_id.clone(), call);
    }

    pub fn unpark(&self, hitl_id: &str) {
        self.lock().calls.remove(hitl_id);
    }

    /// Oldest first: the one that has waited longest is the one to answer.
    pub fn list(&self) -> Vec<ParkedCall> {
        let mut v: Vec<_> = self.lock().calls.values().cloned().collect();
        v.sort_by_key(|c| c.parked_at);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, task: &str) -> ParkedCall {
        let now = Utc::now();
        ParkedCall {
            hitl_id: id.into(),
            task_id: task.into(),
            tool_name: "bash".into(),
            label: "bash: ls".into(),
            parked_at: now,
            expires_at: now,
        }
    }

    #[test]
    fn no_window_means_refuse_at_once() {
        assert_eq!(Parking::default().remaining("t"), None);
    }

    #[test]
    fn a_deadline_earlier_than_the_window_wins() {
        let p = Parking::default();
        p.open_window("t", Duration::from_secs(3600));
        p.clamp_window("t", Some(Instant::now() + Duration::from_secs(5)));
        let left = p.remaining("t").expect("still open");
        assert!(left <= Duration::from_secs(5), "{left:?}");
    }

    #[test]
    fn a_later_deadline_does_not_widen_the_window() {
        let p = Parking::default();
        p.open_window("t", Duration::from_secs(5));
        p.clamp_window("t", Some(Instant::now() + Duration::from_secs(3600)));
        assert!(p.remaining("t").unwrap() <= Duration::from_secs(5));
    }

    #[test]
    fn a_passed_deadline_closes_the_window() {
        let p = Parking::default();
        p.open_window("t", Duration::from_secs(60));
        p.clamp_window("t", Some(Instant::now()));
        assert_eq!(p.remaining("t"), None);
    }

    #[test]
    fn closing_a_turn_forgets_only_its_calls() {
        let p = Parking::default();
        p.open_window("t1", Duration::from_secs(60));
        p.park(call("a", "t1"));
        p.park(call("b", "t2"));
        p.close_window("t1");
        assert_eq!(p.remaining("t1"), None);
        let left: Vec<_> = p.list().into_iter().map(|c| c.hitl_id).collect();
        assert_eq!(left, vec!["b".to_string()]);
    }
}
