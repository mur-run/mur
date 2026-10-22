//! A run clock that survives a pause (pause/resume design §7).
//!
//! A resumed run must see the time it already spent. Rebuilding an `Instant`
//! as `Instant::now() - spent` can fail (an `Instant` cannot go back past the
//! platform's epoch, which on macOS is boot), and any fallback there would
//! silently hand the run a fresh deadline. So the spent time is carried as a
//! plain `Duration` and added on read.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub(super) struct Stopwatch {
    /// Time already spent before `since` (a resumed run's frozen elapsed).
    base: Duration,
    since: Instant,
}

impl Stopwatch {
    /// Starts at zero, now.
    pub fn fresh() -> Self {
        Self::resumed(Duration::ZERO)
    }

    /// Starts at `base` already elapsed, now.
    pub fn resumed(base: Duration) -> Self {
        Self {
            base,
            since: Instant::now(),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.base.saturating_add(self.since.elapsed())
    }

    /// The instant at which `limit` of total elapsed time is reached. Already
    /// past (`now`) when `base` alone is over it.
    pub fn deadline_at(&self, limit: Duration) -> Instant {
        self.since + limit.saturating_sub(self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resumed_clock_counts_the_time_already_spent() {
        let c = Stopwatch::resumed(Duration::from_secs(600));
        assert!(c.elapsed() >= Duration::from_secs(600));
        assert!(c.elapsed() < Duration::from_secs(601));
    }

    #[test]
    fn the_deadline_is_what_remains_not_a_fresh_allowance() {
        let c = Stopwatch::resumed(Duration::from_secs(50));
        let at = c.deadline_at(Duration::from_secs(60));
        let left = at.saturating_duration_since(Instant::now());
        assert!(left <= Duration::from_secs(10), "{left:?}");
        assert!(left > Duration::from_secs(9), "{left:?}");
    }

    #[test]
    fn a_clock_already_past_its_limit_is_due_now() {
        let c = Stopwatch::resumed(Duration::from_secs(90));
        let at = c.deadline_at(Duration::from_secs(60));
        assert!(at <= Instant::now());
    }
}
