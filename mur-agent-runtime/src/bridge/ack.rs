//! Cursor-advancement helper. **`committed_offset` advances if and only if
//! the user agent returns 2xx.** Generic over `T` so platforms with
//! non-numeric cursors (e.g. Slack `next_cursor: String`) can use it.
//!
//! Concrete bridges MUST persist `committed_offset` to disk before resuming
//! polling so a crash mid-pending does not skip messages.

#[derive(Debug)]
pub struct AckTracker<T: Clone + PartialEq> {
    committed: T,
    pending: Option<T>,
}

impl<T: Clone + PartialEq> AckTracker<T> {
    pub fn new(initial: T) -> Self {
        Self {
            committed: initial,
            pending: None,
        }
    }
    pub fn committed_offset(&self) -> T {
        self.committed.clone()
    }
    /// Idempotent: a second `start_pending` without intervening confirm/reject
    /// replaces the first.
    pub fn start_pending(&mut self, next: T) {
        self.pending = Some(next);
    }
    pub fn confirm(&mut self) {
        if let Some(p) = self.pending.take() {
            self.committed = p;
        }
    }
    pub fn reject(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirm_advances() {
        let mut t = AckTracker::new(100);
        t.start_pending(105);
        assert_eq!(t.committed_offset(), 100);
        t.confirm();
        assert_eq!(t.committed_offset(), 105);
    }
    #[test]
    fn reject_keeps_old() {
        let mut t = AckTracker::new(100);
        t.start_pending(105);
        t.reject();
        assert_eq!(t.committed_offset(), 100);
    }
}
