//! Retry policy executor.

use mur_common::RetryPolicy;
use mur_common::agent::BackoffStrategy;
use std::future::Future;

pub type Classifier<E> = Box<dyn Fn(&E) -> &'static str + Send + Sync>;

pub async fn run_with_retry<T, E, F, Fut>(
    policy: &RetryPolicy,
    classifier: Classifier<E>,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                attempt += 1;
                let kind = classifier(&e);
                if attempt > policy.max_retries || !policy.retry_on.iter().any(|k| k == kind) {
                    return Err(e);
                }
                let delay = compute_delay(policy, attempt);
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn compute_delay(policy: &RetryPolicy, attempt: u32) -> std::time::Duration {
    let base = policy.initial_delay_ms;
    let cap = policy.max_delay_ms.unwrap_or(u64::MAX);
    let ms = match policy.backoff {
        BackoffStrategy::Fixed => base,
        BackoffStrategy::Linear => base.saturating_mul(attempt as u64),
        BackoffStrategy::Exponential => base.saturating_mul(1u64 << (attempt - 1).min(20)),
    }
    .min(cap);
    std::time::Duration::from_millis(ms)
}
