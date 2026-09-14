//! Poll intervals (spec §排程、租約與退避). Two tables: `pending` for a
//! source that answered, `unknown` for a source that did not — the second is
//! shorter and capped lower because it is our problem to notice quickly, not
//! the work's.

use std::time::Duration;

/// 30s → 1m → 2m → 5m → 15m → 30m, then held at 30m.
const PENDING_STEPS_SECS: &[u64] = &[30, 60, 120, 300, 900, 1800];
/// 10s → 30s → 1m → 5m, then held at 5m.
const UNKNOWN_STEPS_SECS: &[u64] = &[10, 30, 60, 300];
/// No adapter recommendation may schedule a check sooner than this.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);
/// Read-only cadence after a hard deadline when monitoring is retained.
pub const RETAIN_INTERVAL: Duration = Duration::from_secs(2 * 3600);
/// Consecutive `unknown` observations before a `monitor_unhealthy` event.
pub const UNHEALTHY_AFTER_UNKNOWN: u32 = 6;
/// ± this percentage of the base interval.
const JITTER_PCT: u64 = 20;

pub fn pending_delay(attempt: u32) -> Duration {
    step(PENDING_STEPS_SECS, attempt)
}

pub fn unknown_delay(streak: u32) -> Duration {
    step(UNKNOWN_STEPS_SECS, streak)
}

fn step(table: &[u64], i: u32) -> Duration {
    let idx = (i as usize).min(table.len() - 1);
    Duration::from_secs(table[idx])
}

/// Deterministic jitter in `[base − 20 %, base + 20 %]`. Deterministic so a
/// test can assert an exact `next_check_at`; spread so a herd of monitors
/// created together does not poll together.
pub fn with_jitter(base: Duration, seed: u64) -> Duration {
    let span = 2 * JITTER_PCT + 1;
    let offset_pct = (seed % span) as i64 - JITTER_PCT as i64;
    let base_ms = base.as_millis() as i64;
    let jittered = base_ms + base_ms * offset_pct / 100;
    Duration::from_millis(jittered.max(0) as u64)
}

/// An adapter's `recommended_poll_after` (e.g. GitHub `Retry-After`) may
/// lengthen a wait but never shorten it below the global floor.
pub fn clamp_recommended(recommended: Option<Duration>, computed: Duration) -> Duration {
    match recommended {
        Some(r) => r.max(MIN_INTERVAL),
        None => computed.max(MIN_INTERVAL),
    }
}

/// FNV-1a over `monitor_id` and `attempt` — stable across processes, no dep.
pub fn seed(monitor_id: &str, attempt: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in monitor_id.bytes().chain(attempt.to_le_bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_follows_the_spec_sequence_then_holds() {
        let want = [30, 60, 120, 300, 900, 1800, 1800, 1800];
        for (i, secs) in want.iter().enumerate() {
            assert_eq!(
                pending_delay(i as u32),
                Duration::from_secs(*secs),
                "attempt {i}"
            );
        }
    }

    #[test]
    fn unknown_backoff_is_shorter_and_capped() {
        assert_eq!(unknown_delay(0), Duration::from_secs(10));
        assert_eq!(unknown_delay(3), Duration::from_secs(300));
        assert_eq!(unknown_delay(50), Duration::from_secs(300));
        assert!(unknown_delay(50) < pending_delay(50));
    }

    #[test]
    fn jitter_is_bounded_and_deterministic() {
        let base = Duration::from_secs(1000);
        for seed in 0..500u64 {
            let j = with_jitter(base, seed);
            assert!(
                j >= Duration::from_secs(800) && j <= Duration::from_secs(1200),
                "{j:?}"
            );
        }
        assert_eq!(with_jitter(base, 7), with_jitter(base, 7));
        assert_eq!(seed("m1", 3), seed("m1", 3));
        assert_ne!(seed("m1", 3), seed("m1", 4));
    }

    #[test]
    fn recommended_poll_after_cannot_beat_the_floor() {
        let computed = Duration::from_secs(120);
        assert_eq!(clamp_recommended(None, computed), computed);
        assert_eq!(
            clamp_recommended(Some(Duration::from_secs(1)), computed),
            MIN_INTERVAL
        );
        assert_eq!(
            clamp_recommended(Some(Duration::from_secs(600)), computed),
            Duration::from_secs(600)
        );
    }
}
