//! Anthropic rate-limit header parsing.
//!
//! Spec §4.11 + §6.3.

use chrono::{DateTime, Duration, Utc};
use http::HeaderMap;

#[derive(Debug, Clone)]
pub enum ResumeStrategy {
    /// Sleep this duration relative to now.
    After(Duration),
    /// Sleep until this absolute timestamp.
    AtTimestamp(DateTime<Utc>),
    /// No header info — caller falls back to full-jitter backoff.
    Backoff { attempt: u8 },
}

const ANTHROPIC_RATELIMIT_RESET_HEADERS: &[&str] = &[
    "anthropic-ratelimit-requests-reset",
    "anthropic-ratelimit-tokens-reset",
    "anthropic-ratelimit-input-tokens-reset",
    "anthropic-ratelimit-output-tokens-reset",
];

/// Parse an Anthropic 429 (or 529 overloaded) response into a resume strategy.
///
/// Order of precedence:
///   1. `retry-after` header (seconds OR HTTP-date)
///   2. `anthropic-ratelimit-{requests,tokens,input-tokens,output-tokens}-reset`
///      (RFC 3339); take the MAX-reset across buckets (we have to wait for the slowest).
///   3. Fallback: ResumeStrategy::Backoff { attempt: 0 }
///
/// `status_code = 529` (overloaded_error) multiplies the chosen wait by 6
/// (per spec, range 4-8x; midpoint chosen). Backoff fallback unaffected.
pub fn parse_anthropic_429(
    headers: &HeaderMap,
    now: DateTime<Utc>,
    status_code: u16,
) -> ResumeStrategy {
    let multiplier: i32 = if status_code == 529 { 6 } else { 1 };

    // 1. retry-after
    if let Some(val) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
        let trimmed = val.trim();
        // try seconds first
        if let Ok(secs) = trimmed.parse::<i64>() {
            return ResumeStrategy::After(Duration::seconds(secs * multiplier as i64));
        }
        // try HTTP-date (RFC 7231 IMF-fixdate). The "GMT" suffix is a literal,
        // not parseable as a timezone by chrono — parse as NaiveDateTime + attach UTC.
        if let Ok(naive) =
            chrono::NaiveDateTime::parse_from_str(trimmed, "%a, %d %b %Y %H:%M:%S GMT")
        {
            let target_utc = naive.and_utc();
            if multiplier > 1 {
                let delta = target_utc - now;
                return ResumeStrategy::After(delta * multiplier);
            }
            return ResumeStrategy::AtTimestamp(target_utc);
        }
    }

    // 2. anthropic-ratelimit-*-reset → take MAX
    let mut max_reset: Option<DateTime<Utc>> = None;
    for name in ANTHROPIC_RATELIMIT_RESET_HEADERS {
        if let Some(v) = headers.get(*name).and_then(|v| v.to_str().ok())
            && let Ok(t) = chrono::DateTime::parse_from_rfc3339(v)
        {
            let t_utc = t.with_timezone(&Utc);
            max_reset = Some(max_reset.map(|m| m.max(t_utc)).unwrap_or(t_utc));
        }
    }
    if let Some(reset) = max_reset {
        if multiplier > 1 {
            let delta = reset - now;
            return ResumeStrategy::After(delta * multiplier);
        }
        return ResumeStrategy::AtTimestamp(reset);
    }

    // 3. fallback
    ResumeStrategy::Backoff { attempt: 0 }
}
