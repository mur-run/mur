//! Capture `tracing` output inside a test, without losing lines to the
//! process-wide callsite-interest cache (#2025, #2035).
//!
//! # Why a plain `with_default` capture is flaky
//!
//! tracing-core caches each callsite's interest the first time the callsite
//! is hit. While only one dispatcher is live in the process it takes a
//! shortcut and asks just the *hitting thread's* default dispatcher. Under
//! `cargo test` many tests share one process, so a sibling test running on a
//! subscriber-less thread can hit a callsite first, cache `never`, and the
//! capturing test then sees nothing. A test asserting that a line *is*
//! logged fails; a test asserting that the log is *empty* passes for the
//! wrong reason. nextest (one process per test) never sees this, which is
//! why CI stays green while local runs flake.
//!
//! [`capture`] keeps a second, inert dispatcher alive for the duration of the
//! capture. With two live dispatchers tracing-core asks every one of them,
//! the answers disagree, and the callsite is cached `sometimes` — re-checked
//! on each hit against the current thread's subscriber, ours included.
//!
//! Its own crate because the tests that need it live in `mur-agent-runtime`,
//! `mur-channel` and `mur-core`, and the guard is subtle enough that copies
//! would drift. It depends on nothing but `tracing`, so any crate may use it.

use std::sync::{Arc, Mutex};

use tracing::Level;
use tracing::subscriber::NoSubscriber;

/// Run `f` with a capturing `fmt` subscriber as the thread default and
/// return its result together with everything logged at `max_level` or
/// more severe. ANSI colouring is off, so assertions can match `key=value`.
pub fn capture<R>(max_level: Level, f: impl FnOnce() -> R) -> (R, String) {
    let buf = Buf::default();
    let writer = buf.clone();
    // Held, not used: its only job is to be a second live dispatcher while
    // `f` runs. Declared before the subscriber so it outlives the call.
    let _second = tracing::Dispatch::new(NoSubscriber::default());
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(max_level)
        .with_ansi(false)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    (out, buf.text())
}

/// In-memory writer shared between the subscriber and the caller.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Buf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A callsite only this test hits, so its interest starts uncached.
    fn probe() {
        tracing::warn!("mur-test-log guarded probe");
    }

    /// The race, forced deterministically: a thread with no subscriber hits
    /// the callsite first (spawn + join, no timing), then the capturing
    /// thread hits it. Guarded, the line is still captured exactly once.
    #[test]
    fn a_callsite_first_hit_without_a_subscriber_is_still_captured() {
        let ((), log) = capture(Level::WARN, || {
            std::thread::spawn(probe).join().unwrap();
            probe();
        });
        let hits = log.matches("mur-test-log guarded probe").count();
        assert_eq!(hits, 1, "exactly one captured line; log: {log}");
    }

    #[test]
    fn capture_filters_by_level_and_returns_the_closure_result() {
        let (n, log) = capture(Level::WARN, || {
            tracing::info!("below the bar");
            tracing::warn!(answer = 42, "at the bar");
            7
        });
        assert_eq!(n, 7);
        assert!(
            log.contains("at the bar") && log.contains("answer=42"),
            "{log}"
        );
        assert!(!log.contains("below the bar"), "{log}");
    }
}
