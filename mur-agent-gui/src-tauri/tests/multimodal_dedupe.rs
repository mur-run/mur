use mur_agent_gui_lib::multimodal::DropDeduper;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[test]
fn duplicate_within_window_is_filtered() {
    let mut d = DropDeduper::with_window(Duration::from_millis(100));
    let paths = vec![PathBuf::from("/a"), PathBuf::from("/b")];
    let t0 = Instant::now();
    assert!(d.observe(&paths, t0));
    assert!(!d.observe(&paths, t0 + Duration::from_millis(40)));
    assert!(!d.observe(&paths, t0 + Duration::from_millis(99)));
    assert!(d.observe(&paths, t0 + Duration::from_millis(101)));
}

#[test]
fn order_independent() {
    let mut d = DropDeduper::with_window(Duration::from_millis(100));
    let t0 = Instant::now();
    assert!(d.observe(&[PathBuf::from("/a"), PathBuf::from("/b")], t0));
    // Reversed order — same logical drop.
    assert!(!d.observe(
        &[PathBuf::from("/b"), PathBuf::from("/a")],
        t0 + Duration::from_millis(50)
    ));
}

#[test]
fn different_paths_not_filtered() {
    let mut d = DropDeduper::with_window(Duration::from_millis(100));
    let t0 = Instant::now();
    assert!(d.observe(&[PathBuf::from("/a")], t0));
    assert!(d.observe(&[PathBuf::from("/b")], t0 + Duration::from_millis(20)));
}

#[test]
fn default_constructor() {
    let mut d = DropDeduper::new();
    let t0 = Instant::now();
    assert!(d.observe(&[PathBuf::from("/a")], t0));
}
