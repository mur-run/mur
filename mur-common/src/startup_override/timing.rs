//! Restart and override timing, defined once. The CLI restart wait, the
//! override sweep, the runtime's boot-time age check and the Hub all read
//! these; nothing else may redefine them (`timing_tests` enforces it).

use std::time::Duration;

/// Passive wait for launchd/systemd's natural respawn. Covers respawn latency
/// (old-process shutdown + ExitTimeOut + any ThrottleInterval) with headroom —
/// real launchd respawn can take ~2 min after rapid restarts.
pub const RESPAWN_WAIT_SECS: u64 = 120;

/// Extra patience for a slow cold start (sandbox apply + model load). Only
/// spent while a runnable fresh process is alive — a process that dies fails
/// fast instead of burning the whole window, and a wrongly-"runnable" zombie
/// still gets kicked once the ceiling expires.
pub const SLOW_START_WAIT_SECS: u64 = 240;

/// Extra grace period added on top of the runtime's `stop_timeout_secs` before
/// the SIGKILL fallback fires.  The SIGKILL wait MUST outlast the runtime's own
/// drain bound so we never abort a turn mid-flight.
pub const RESTART_KILL_GRACE_SECS: u64 = 5;

/// How long a writer waits for a boot to acknowledge its override nonce:
/// the passive respawn wait, then a slow start before and after the kick.
/// Nominal — the wait checks it between polls.
pub const OVERRIDE_ACK_WAIT_SECS: u64 = RESPAWN_WAIT_SECS + 2 * SLOW_START_WAIT_SECS;

/// Slack for a wall-clock step, fsync and scheduling between the writer's
/// `created_at` and the runtime's check. Belongs to [`override_max_age`] only.
pub const OVERRIDE_AGE_MARGIN_SECS: u64 = 60;

const _: () = assert!(OVERRIDE_ACK_WAIT_SECS >= RESPAWN_WAIT_SECS + OVERRIDE_AGE_MARGIN_SECS);

/// Oldest `created_at` a boot may apply: publish → drain + SIGKILL grace →
/// the whole acknowledgement window (including the kick) → margin.
pub fn override_max_age(stop_timeout_secs: u64) -> Duration {
    Duration::from_secs(
        stop_timeout_secs
            + RESTART_KILL_GRACE_SECS
            + OVERRIDE_ACK_WAIT_SECS
            + OVERRIDE_AGE_MARGIN_SECS,
    )
}
