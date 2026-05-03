use mur_agent_runtime::bridge::ack::AckTracker;

fn simulate(t: &mut AckTracker<u64>, ok: bool, high: u64) {
    t.start_pending(high);
    if ok {
        t.confirm();
    } else {
        t.reject();
    }
}

#[test]
fn five_xx_then_two_xx_recovers() {
    let mut t = AckTracker::new(0u64);
    simulate(&mut t, false, 10);
    assert_eq!(t.committed_offset(), 0);
    simulate(&mut t, true, 10);
    assert_eq!(t.committed_offset(), 10);
    simulate(&mut t, true, 20);
    assert_eq!(t.committed_offset(), 20);
}

#[test]
fn many_failures_pin_offset() {
    let mut t = AckTracker::new(50u64);
    for _ in 0..10 {
        simulate(&mut t, false, 60);
    }
    assert_eq!(t.committed_offset(), 50);
}
