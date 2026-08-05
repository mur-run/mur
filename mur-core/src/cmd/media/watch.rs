//! Watch-session mutators behind the MCP `watch_*` tools. These only flip flags in
//! `watch.json`; the runtime `WatchScheduler` observes them (spec §3, §6).
// These functions are consumed by mur-mcp-server (cross-crate); the mur binary
// does not call them directly, so suppress the dead_code lint for this module.
#![allow(dead_code)]

use mur_common::media::{Consent, WatchSession, load_watch, save_watch};
use std::path::Path;

/// Start (or restart) a proactive watch session: active, unmuted, consent reset.
pub fn start(mur_home: &Path) -> std::io::Result<WatchSession> {
    let mut s = load_watch(mur_home);
    s.active = true;
    s.muted = false;
    s.consent = Consent::Unasked;
    s.last_interjection_ms = 0;
    s.last_scene_phash = 0;
    save_watch(mur_home, &s)?;
    Ok(s)
}

/// Stop the session (no further interjections).
pub fn stop(mur_home: &Path) -> std::io::Result<WatchSession> {
    let mut s = load_watch(mur_home);
    s.active = false;
    save_watch(mur_home, &s)?;
    Ok(s)
}

/// Silence interjections ("噓") without ending the session.
pub fn mute(mur_home: &Path) -> std::io::Result<WatchSession> {
    let mut s = load_watch(mur_home);
    s.muted = true;
    save_watch(mur_home, &s)?;
    Ok(s)
}

/// Current session snapshot.
pub fn status(mur_home: &Path) -> WatchSession {
    load_watch(mur_home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn start_then_mute_then_stop() {
        let home = TempDir::new().unwrap();
        let s = start(home.path()).unwrap();
        assert!(s.active && !s.muted);
        let s = mute(home.path()).unwrap();
        assert!(s.active && s.muted);
        let s = stop(home.path()).unwrap();
        assert!(!s.active && s.muted);
        assert!(!status(home.path()).active);
    }
}
