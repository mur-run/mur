//! Startup safety checks for the versioned knowledge and agents stores.
//!
//! Spec §4.2.7: on each daemon start, verify both git repos are healthy,
//! auto-rebuild the index if HEAD diverged externally, and run `git gc
//! --auto` at most once every 7 days.
//!
//! All checks are best-effort: failures emit warnings but never crash the
//! daemon.

use std::path::Path;

use mur_core::store::versioned::{VersionedYamlStore, agent::VersionedAgentStore};

const GC_INTERVAL_DAYS: u64 = 7;
const GC_MARKER_FILE: &str = ".last-gc";

/// Run all startup safety checks. Logs warnings on stderr; never panics.
pub fn run(mur_dir: &Path) {
    check_knowledge(mur_dir);
    check_agents(&mur_dir.join("agents"));
    gc_if_due(mur_dir);
}

fn check_knowledge(mur_dir: &Path) {
    if !mur_dir.join(".git").exists() {
        return;
    }
    match VersionedYamlStore::open(mur_dir) {
        Err(e) => {
            eprintln!("murmurd: knowledge store unhealthy ({e:#}) — versioned history unavailable");
        }
        Ok(mut store) => match store.detect_external_change() {
            Ok(true) => {
                eprintln!(
                    "murmurd: knowledge HEAD diverged from index (external git op?) — rebuilding"
                );
                if let Err(e) = store.rebuild_index() {
                    eprintln!("murmurd: knowledge index rebuild failed: {e:#}");
                }
            }
            Ok(false) => {}
            Err(e) => eprintln!("murmurd: knowledge change-detect failed: {e:#}"),
        },
    }
}

fn check_agents(agents_dir: &Path) {
    if !agents_dir.join(".git").exists() {
        return;
    }
    match VersionedAgentStore::open(agents_dir) {
        Err(e) => {
            eprintln!("murmurd: agents store unhealthy ({e:#}) — versioned history unavailable");
        }
        Ok(mut store) => match store.detect_external_change() {
            Ok(true) => {
                eprintln!(
                    "murmurd: agents HEAD diverged from index (external git op?) — rebuilding"
                );
                if let Err(e) = store.rebuild_index() {
                    eprintln!("murmurd: agents index rebuild failed: {e:#}");
                }
            }
            Ok(false) => {}
            Err(e) => eprintln!("murmurd: agents change-detect failed: {e:#}"),
        },
    }
}

/// Spawn `git gc --auto` in `mur_dir` at most once every 7 days.
/// Uses a marker file `<mur_dir>/.last-gc` to track the last run time.
fn gc_if_due(mur_dir: &Path) {
    let marker = mur_dir.join(GC_MARKER_FILE);
    if let Ok(meta) = std::fs::metadata(&marker)
        && let Ok(modified) = meta.modified()
    {
        let age = std::time::SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        if age.as_secs() < GC_INTERVAL_DAYS * 86_400 {
            return;
        }
    }

    // Spawn detached — don't block daemon startup. std::process::Child has no
    // built-in reaper (unlike tokio's), so wait for it on a background thread
    // instead of dropping the handle, or it becomes a zombie on exit.
    let gc_ok = std::process::Command::new("git")
        .args(["-C", &mur_dir.to_string_lossy(), "gc", "--auto", "--quiet"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|mut child| {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        })
        .is_ok();

    if gc_ok {
        let _ = std::fs::write(&marker, b"");
    }
}
