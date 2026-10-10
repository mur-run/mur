//! Time checks on the signed `issued_at` of HITL payloads (#1764 option C).
//!
//! Both `issued_at` values are inside the signature, so nobody without the
//! router's key can move them. What these checks guard against is not an
//! attacker but disagreeing clocks: the request is stamped by the router
//! host, the answer possibly by a phone. [`HITL_CLOCK_SKEW_SECS`] is the
//! tolerance for that disagreement.

use chrono::{DateTime, Duration, Utc};

use super::APPROVAL_TTL_SECS;

/// Clock-skew tolerance between HITL writers, in seconds.
///
/// **A fault tolerance, not a security boundary.** Every time it widens is
/// signed, so shrinking it buys no security; it only decides how far apart
/// two honest clocks may drift before a legitimate answer is refused. It
/// covers NTP-level drift (seconds), not a clock set by hand: an answer
/// from a device whose clock is days off is refused, by design. Keep it
/// small — a large value makes "answered before it was asked" meaningless.
///
/// Used by all three checks in [`check_answer_time`], with this one meaning:
/// - from the future: `issued_at <= now + skew` (a fast clock cannot sign an
///   answer that stays valid into the future);
/// - answered before asked: `answer >= request - skew` (only the early
///   direction gets slack; late is normal);
/// - expired: `answer - request <= TTL + skew` (skew is added to the TTL,
///   never subtracted from `now`).
pub const HITL_CLOCK_SKEW_SECS: i64 = 60;

/// Why a signed time rules an answer out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFault {
    /// Stamped later than `now + skew`.
    FromTheFuture,
    /// The answer predates its request by more than the skew.
    BeforeRequest,
    /// The answer came more than `TTL + skew` after its request.
    AfterExpiry,
}

/// Is `answered` a timely answer to a request `asked`, judged at `now`?
pub fn check_answer_time(
    asked: DateTime<Utc>,
    answered: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), TimeFault> {
    let skew = Duration::seconds(HITL_CLOCK_SKEW_SECS);
    if asked > now + skew || answered > now + skew {
        return Err(TimeFault::FromTheFuture);
    }
    if answered < asked - skew {
        return Err(TimeFault::BeforeRequest);
    }
    if answered - asked > Duration::seconds(APPROVAL_TTL_SECS) + skew {
        return Err(TimeFault::AfterExpiry);
    }
    Ok(())
}

/// Is something signed at `issued_at` still within the TTL at `now`?
///
/// Two uses, one rule: a request is still answerable (the rule an answer
/// stamped `now` would face, so `mur channel approve` and the gate agree on
/// which requests are open), and a settled decision may still be reused.
pub fn check_fresh(issued_at: DateTime<Utc>, now: DateTime<Utc>) -> Result<(), TimeFault> {
    check_answer_time(issued_at, now, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: i64) -> Duration {
        Duration::seconds(s)
    }

    const SKEW: i64 = HITL_CLOCK_SKEW_SECS;

    #[test]
    fn an_answer_a_little_before_its_request_is_clock_noise() {
        let now = Utc::now();
        let asked = now - secs(10);
        assert_eq!(check_answer_time(asked, asked - secs(SKEW), now), Ok(()));
        assert_eq!(
            check_answer_time(asked, asked - secs(SKEW + 1), now),
            Err(TimeFault::BeforeRequest)
        );
    }

    #[test]
    fn expiry_adds_the_skew_to_the_ttl() {
        let now = Utc::now();
        let asked = now - secs(APPROVAL_TTL_SECS + SKEW + 10);
        let edge = asked + secs(APPROVAL_TTL_SECS + SKEW);
        assert_eq!(check_answer_time(asked, edge, now), Ok(()));
        assert_eq!(
            check_answer_time(asked, edge + secs(1), now),
            Err(TimeFault::AfterExpiry)
        );
    }

    #[test]
    fn a_fast_clock_is_bounded_by_the_skew() {
        let now = Utc::now();
        assert_eq!(check_answer_time(now, now + secs(SKEW), now), Ok(()));
        assert_eq!(
            check_answer_time(now, now + secs(SKEW + 1), now),
            Err(TimeFault::FromTheFuture)
        );
        assert_eq!(
            check_answer_time(now + secs(SKEW + 1), now + secs(SKEW + 1), now),
            Err(TimeFault::FromTheFuture),
            "a future-dated request is refused too"
        );
        // Thirty days fast: the exact failure the bound exists for.
        assert_eq!(
            check_answer_time(now, now + Duration::days(30), now),
            Err(TimeFault::FromTheFuture)
        );
    }

    #[test]
    fn fresh_until_ttl_plus_skew_and_never_from_the_future() {
        let now = Utc::now();
        assert_eq!(
            check_fresh(now - secs(APPROVAL_TTL_SECS + SKEW), now),
            Ok(())
        );
        assert_eq!(
            check_fresh(now - secs(APPROVAL_TTL_SECS + SKEW + 1), now),
            Err(TimeFault::AfterExpiry)
        );
        assert_eq!(
            check_fresh(now + secs(SKEW + 1), now),
            Err(TimeFault::FromTheFuture)
        );
    }
}
