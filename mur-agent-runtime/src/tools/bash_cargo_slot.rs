//! Cross-process cap on concurrent cargo builds.
//!
//! Every turn builds into its own `CARGO_TARGET_DIR`, so Cargo's own target
//! lock never serialises two turns. On a 16 GB host three turns linking
//! `mur_core` at once (~1.2 GB per `ld`) pushed the box deep into swap. A
//! fixed set of flock'd slot files under `<mur_home>/locks/cargo/` is shared
//! by every agent runtime on the machine: a `bash` command that runs cargo
//! takes a slot before it spawns and holds it until the shell exits. The OS
//! drops a flock when its holder dies, so a crashed runtime never leaks one.

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `<mur_home>/<this>/` holds the slot files.
pub const SLOT_DIR: &[&str] = &["locks", "cargo"];
/// One concurrent build per this many GiB of physical memory (auto mode).
pub const GIB_PER_SLOT: u64 = 8;
/// Never fewer slots than this, whatever the memory probe says.
pub const MIN_SLOTS: u32 = 1;
/// How often a waiter re-tries the slot files.
const POLL: Duration = Duration::from_millis(500);

/// A held slot. Dropping it releases the flock.
#[derive(Debug)]
pub struct CargoSlot {
    _file: File,
    pub index: u32,
}

/// The slot count to enforce: `configured` when non-zero, else derived
/// from physical memory.
pub fn effective_slots(configured: u32) -> u32 {
    if configured > 0 {
        return configured;
    }
    slots_for_memory(physical_memory_bytes())
}

/// `bytes / GIB_PER_SLOT GiB`, at least [`MIN_SLOTS`]; unknown memory → minimum.
pub fn slots_for_memory(bytes: Option<u64>) -> u32 {
    let per = GIB_PER_SLOT * 1024 * 1024 * 1024;
    let n = bytes.map(|b| b / per).unwrap_or(0);
    u32::try_from(n).unwrap_or(u32::MAX).max(MIN_SLOTS)
}

#[cfg(unix)]
fn physical_memory_bytes() -> Option<u64> {
    // SAFETY: sysconf only reads system constants.
    let (pages, size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    (pages > 0 && size > 0).then(|| pages as u64 * size as u64)
}

#[cfg(not(unix))]
fn physical_memory_bytes() -> Option<u64> {
    None
}

pub fn slot_dir(mur_home: &Path) -> PathBuf {
    SLOT_DIR
        .iter()
        .fold(mur_home.to_path_buf(), |p, c| p.join(c))
}

/// One non-blocking pass over the `slots` files.
pub fn try_acquire(dir: &Path, slots: u32) -> std::io::Result<Option<CargoSlot>> {
    std::fs::create_dir_all(dir)?;
    for index in 0..slots {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("slot-{index}.lock")))?;
        if file.try_lock_exclusive().is_ok() {
            return Ok(Some(CargoSlot { _file: file, index }));
        }
    }
    Ok(None)
}

/// Wait up to `within` for a slot. `Ok(None)` = every slot stayed busy.
pub async fn acquire(
    dir: &Path,
    slots: u32,
    within: Duration,
) -> std::io::Result<Option<CargoSlot>> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(s) = try_acquire(dir, slots)? {
            return Ok(Some(s));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The model-facing refusal when no slot freed up in time.
pub fn busy_message(slots: u32, waited: Duration) -> String {
    format!(
        "cargo is limited to {slots} concurrent build(s) across all MUR agents on this machine \
         (turn_track.max_parallel_cargo), and none freed up within {}s. Nothing was run. \
         Retry with a larger timeout_secs, or finish/kill your own running cargo job first.",
        waited.as_secs()
    )
}

#[cfg(test)]
#[path = "bash_cargo_slot_tests.rs"]
mod tests;
