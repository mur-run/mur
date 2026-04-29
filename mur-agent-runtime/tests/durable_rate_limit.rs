use mur_agent_runtime::durable::rate_limit::{parse_anthropic_429, ResumeStrategy};

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[test]
fn retry_after_seconds() {
    let mut h = http::HeaderMap::new();
    h.insert("retry-after", "42".parse().unwrap());
    let s = parse_anthropic_429(&h, now(), 429);
    match s {
        ResumeStrategy::After(d) => assert_eq!(d.num_seconds(), 42),
        _ => panic!("expected After(42s), got {s:?}"),
    }
}

#[test]
fn retry_after_http_date() {
    let mut h = http::HeaderMap::new();
    let n = chrono::Utc::now();
    let target = n + chrono::Duration::seconds(120);
    let formatted = target.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    h.insert("retry-after", formatted.parse().unwrap());
    let s = parse_anthropic_429(&h, n, 429);
    match s {
        ResumeStrategy::AtTimestamp(t) => {
            let delta = (t - target).num_seconds().abs();
            assert!(delta <= 1, "expected ~target, got delta {delta}s");
        }
        ResumeStrategy::After(d) => {
            let secs = d.num_seconds();
            assert!((118..=122).contains(&secs), "expected ~120s, got {secs}");
        }
        _ => panic!("expected AtTimestamp or After, got {s:?}"),
    }
}

#[test]
fn ratelimit_reset_used_when_no_retry_after() {
    let mut h = http::HeaderMap::new();
    let reset = chrono::Utc::now() + chrono::Duration::seconds(120);
    h.insert(
        "anthropic-ratelimit-tokens-reset",
        reset.to_rfc3339().parse().unwrap(),
    );
    let s = parse_anthropic_429(&h, now(), 429);
    match s {
        ResumeStrategy::AtTimestamp(t) => {
            let delta = (t - reset).num_seconds().abs();
            assert!(delta <= 1, "expected ≈ reset, got delta {delta}s");
        }
        _ => panic!("expected AtTimestamp, got {s:?}"),
    }
}

#[test]
fn ratelimit_reset_takes_max_across_buckets() {
    let mut h = http::HeaderMap::new();
    let n = chrono::Utc::now();
    let r1 = n + chrono::Duration::seconds(60);
    let r2 = n + chrono::Duration::seconds(180);
    h.insert(
        "anthropic-ratelimit-requests-reset",
        r1.to_rfc3339().parse().unwrap(),
    );
    h.insert(
        "anthropic-ratelimit-tokens-reset",
        r2.to_rfc3339().parse().unwrap(),
    );
    let s = parse_anthropic_429(&h, n, 429);
    match s {
        ResumeStrategy::AtTimestamp(t) => {
            let delta = (t - r2).num_seconds().abs();
            assert!(delta <= 1, "expected slowest reset (r2), got delta {delta}s");
        }
        _ => panic!("expected AtTimestamp, got {s:?}"),
    }
}

#[test]
fn fallback_full_jitter_backoff() {
    let h = http::HeaderMap::new();
    let s = parse_anthropic_429(&h, now(), 429);
    assert!(matches!(s, ResumeStrategy::Backoff { attempt: 0 }), "got {s:?}");
}

#[test]
fn five_two_nine_multiplies_wait() {
    let mut h = http::HeaderMap::new();
    h.insert("retry-after", "10".parse().unwrap());
    let s = parse_anthropic_429(&h, now(), 529);
    match s {
        ResumeStrategy::After(d) => {
            let secs = d.num_seconds();
            assert!((40..=80).contains(&secs), "expected 4-8x of 10s, got {secs}");
        }
        _ => panic!("expected After(multiplied), got {s:?}"),
    }
}

#[test]
fn five_two_nine_no_headers_falls_through_to_backoff_unchanged() {
    let h = http::HeaderMap::new();
    let s = parse_anthropic_429(&h, now(), 529);
    assert!(matches!(s, ResumeStrategy::Backoff { attempt: 0 }), "got {s:?}");
}
