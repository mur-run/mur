use super::*;

const GIB: u64 = 1024 * 1024 * 1024;

#[test]
fn slots_follow_memory() {
    assert_eq!(slots_for_memory(None), MIN_SLOTS);
    assert_eq!(slots_for_memory(Some(4 * GIB)), MIN_SLOTS);
    assert_eq!(slots_for_memory(Some(16 * GIB)), 2);
    assert_eq!(slots_for_memory(Some(64 * GIB)), 8);
}

#[test]
fn configured_value_wins() {
    assert_eq!(effective_slots(3), 3);
    assert!(effective_slots(0) >= MIN_SLOTS);
}

/// The bug: N turns could all build at once. With N slots the N+1th waits.
#[test]
fn third_build_is_refused_while_two_hold_slots() {
    let dir = tempfile::tempdir().unwrap();
    let a = try_acquire(dir.path(), 2).unwrap().expect("first slot");
    let b = try_acquire(dir.path(), 2).unwrap().expect("second slot");
    assert_ne!(a.index, b.index);
    assert!(try_acquire(dir.path(), 2).unwrap().is_none());
    drop(a);
    assert!(try_acquire(dir.path(), 2).unwrap().is_some());
}

#[tokio::test]
async fn waiter_gets_the_slot_when_it_frees() {
    let dir = tempfile::tempdir().unwrap();
    let held = try_acquire(dir.path(), 1).unwrap().unwrap();
    let path = dir.path().to_path_buf();
    let waiter = tokio::spawn(async move { acquire(&path, 1, Duration::from_secs(5)).await });
    tokio::time::sleep(Duration::from_millis(700)).await;
    drop(held);
    assert!(waiter.await.unwrap().unwrap().is_some());
}

#[tokio::test]
async fn gives_up_after_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let _held = try_acquire(dir.path(), 1).unwrap().unwrap();
    let got = acquire(dir.path(), 1, Duration::from_millis(100))
        .await
        .unwrap();
    assert!(got.is_none());
}
