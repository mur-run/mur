use mur_agent_runtime::retry::{Classifier, run_with_retry};
use mur_common::RetryPolicy;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn succeeds_after_two_retries() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    let policy = RetryPolicy {
        max_retries: 3,
        backoff: mur_common::agent::BackoffStrategy::Exponential,
        initial_delay_ms: 10,
        max_delay_ms: Some(100),
        retry_on: vec!["transient".into()],
    };
    let classifier: Classifier<()> = Box::new(|_| "transient");
    let result = run_with_retry(&policy, classifier, || {
        let a = a.clone();
        async move {
            let n = a.fetch_add(1, Ordering::SeqCst) + 1;
            if n < 3 { Err(()) } else { Ok("done") }
        }
    })
    .await;
    assert_eq!(result.unwrap(), "done");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let policy = RetryPolicy {
        max_retries: 2,
        backoff: mur_common::agent::BackoffStrategy::Fixed,
        initial_delay_ms: 5,
        max_delay_ms: None,
        retry_on: vec!["x".into()],
    };
    let classifier: Classifier<()> = Box::new(|_| "x");
    let res: Result<&'static str, ()> =
        run_with_retry(&policy, classifier, || async { Err(()) }).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn unmatched_kind_does_not_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    let policy = RetryPolicy {
        max_retries: 3,
        backoff: mur_common::agent::BackoffStrategy::Fixed,
        initial_delay_ms: 1,
        max_delay_ms: None,
        retry_on: vec!["rate_limit".into()],
    };
    let classifier: Classifier<()> = Box::new(|_| "auth_error");
    let res: Result<&'static str, ()> = run_with_retry(&policy, classifier, || {
        let a = a.clone();
        async move {
            a.fetch_add(1, Ordering::SeqCst);
            Err(())
        }
    })
    .await;
    assert!(res.is_err());
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "must not retry non-matching error"
    );
}
