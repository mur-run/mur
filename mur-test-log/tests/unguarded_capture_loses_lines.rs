//! Proof that the guard in `mur_test_log::capture` is load-bearing.
//!
//! The unguarded half only holds while this process has a single live
//! dispatcher, so it lives in its own test binary with exactly one test:
//! no neighbour can register a dispatcher underneath it.
//!
//! If this starts failing after a tracing-core upgrade, the single-dispatcher
//! shortcut has probably gone; check whether the guard is still needed
//! before deleting either.

use tracing::Level;

fn unguarded_probe() {
    tracing::warn!("unguarded probe");
}

fn guarded_probe() {
    tracing::warn!("guarded probe");
}

#[test]
fn without_the_guard_the_line_is_lost_and_with_it_the_line_is_kept() {
    // Unguarded: one dispatcher, so the subscriber-less thread's `never`
    // is cached and the capturing thread's hit is dropped.
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let w = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Sink(w.clone()))
        .with_max_level(Level::WARN)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        std::thread::spawn(unguarded_probe).join().unwrap();
        unguarded_probe();
    });
    let log = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
    assert_eq!(
        log.matches("unguarded probe").count(),
        0,
        "the race no longer reproduces without the guard; log: {log}"
    );

    // Guarded: same steps through `capture`, on a fresh callsite.
    let ((), log) = mur_test_log::capture(Level::WARN, || {
        std::thread::spawn(guarded_probe).join().unwrap();
        guarded_probe();
    });
    assert_eq!(log.matches("guarded probe").count(), 1, "log: {log}");
}

struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
