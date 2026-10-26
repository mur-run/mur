//! Durable record of turn tracks that are open right now (#1909).
//!
//! A track opened by a turn is normally closed by `sweep_turn_track` or the
//! settle path. When the process dies first — SIGKILL, a crash, a shutdown
//! that outlived its grace — nothing in-process can close it, and the log
//! that recorded the open may itself be gone (service-manager logs often live under
//! a temp dir). So every open track gets a small marker file in the agent
//! home, removed when the track closes. A marker found at boot is a track
//! nobody settled: it is reported on every boot until the worktree is gone.
//! This module never deletes a worktree; `track_reclaim` may, but only after
//! saving its diff as a patch (#2152), and the next boot then sees it absent.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Directory under the agent home holding one marker per open track.
pub(super) const OPEN_TRACKS_DIR: &str = "open-turn-tracks";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct OpenMarker {
    pub turn: String,
    pub track_name: String,
    pub track: PathBuf,
    pub project: PathBuf,
    pub opened_at: chrono::DateTime<chrono::Utc>,
}

fn marker_path(agent_home: &Path, track_name: &str) -> PathBuf {
    agent_home
        .join(OPEN_TRACKS_DIR)
        .join(format!("{track_name}.json"))
}

/// Record that `m` is open. Failure is logged, never raised: a turn must not
/// fail because its crash bookkeeping did.
pub(super) fn mark_open(agent_home: &Path, m: &OpenMarker) {
    let path = marker_path(agent_home, &m.track_name);
    let res = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(path.parent().expect("marker has a parent"))?;
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(m)?)?;
        std::fs::rename(&tmp, &path)
    })();
    if let Err(e) = res {
        tracing::warn!(turn = %m.turn, error = %e, "could not record open turn track");
    }
}

/// The track closed (whatever the outcome — a kept track is already reported
/// by the close path), so its marker goes.
pub(super) fn mark_closed(agent_home: &Path, track_name: &str) {
    match std::fs::remove_file(marker_path(agent_home, track_name)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(track_name, error = %e, "could not clear open turn track marker"),
    }
}

/// One marker left behind by a previous process.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Unsettled {
    pub marker: OpenMarker,
    /// Whether the worktree is still on disk. Only a confirmed absence lets
    /// the marker go; an unreadable path is not proof of deletion.
    pub track: TrackState,
}

/// What a boot can say about a marker's worktree.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum TrackState {
    Present,
    /// Confirmed gone: the user cleaned it up.
    Absent,
    /// Could not tell (e.g. permission denied after a filesystem grant was
    /// removed, the sandbox sealed it off, or the project's volume is not
    /// mounted). `Path::exists` would call this absent and delete the only
    /// reminder, so it is kept.
    Unknown,
}

impl TrackState {
    /// A missing worktree only counts as deleted when its project is
    /// reachable: on an unmounted volume the project vanishes too, and that
    /// is "offline", not "cleaned up".
    fn probe(m: &OpenMarker) -> Self {
        match Self::from_probe(m.project.try_exists(), &m.project) {
            Self::Present => Self::from_probe(m.track.try_exists(), &m.track),
            Self::Absent => {
                tracing::warn!(project = %m.project.display(), "project not reachable (volume offline?); marker kept");
                Self::Unknown
            }
            Self::Unknown => Self::Unknown,
        }
    }

    fn from_probe(r: std::io::Result<bool>, path: &Path) -> Self {
        match r {
            Ok(true) => Self::Present,
            Ok(false) => Self::Absent,
            Err(error) => {
                tracing::warn!(track = %path.display(), %error, "cannot check turn track; marker kept");
                Self::Unknown
            }
        }
    }
}

/// Every marker under `agent_home`. Unparseable files are skipped with a
/// warning: a process killed mid-write leaves at most a `.tmp`, but a boot
/// must never fail on a bad marker.
pub(super) fn scan(agent_home: &Path) -> Vec<Unsettled> {
    let dir = agent_home.join(OPEN_TRACKS_DIR);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let parsed = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<OpenMarker>(&b).map_err(|e| e.to_string()));
        match parsed {
            Ok(marker) => {
                let track = TrackState::probe(&marker);
                out.push(Unsettled { marker, track });
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "unreadable open turn track marker; skipped")
            }
        }
    }
    out.sort_by_key(|a| a.marker.opened_at);
    out
}

fn item_title(m: &OpenMarker) -> String {
    format!(
        "Review unsettled turn track {} in {}",
        m.track_name,
        m.project.display()
    )
}

/// Boot-time report: log each leftover track and keep one open item per
/// track so the user sees it after the log is gone.
///
/// The marker lives exactly as long as the worktree. Clearing it on first
/// report meant that closing the open item without removing the worktree
/// silenced the only reminder; now every boot re-checks, re-raises a
/// resolved item while the worktree remains, and only when the worktree is
/// gone resolves the item and drops the marker. Tracks are never touched.
/// Returns how many unsettled tracks are still on disk.
pub(super) fn report_at_boot(agent_home: &Path, mur_home: &Path, agent: &str) -> usize {
    // An unreadable log can't say which items are open, so nothing is
    // cleared or raised against it this boot — markers just wait.
    let live: Option<Vec<String>> = match mur_open_items::try_open(mur_home) {
        Ok(items) => Some(items.into_iter().map(|i| i.title).collect()),
        Err(error) => {
            tracing::warn!(%error, "open-item log unreadable; unsettled-track markers kept");
            None
        }
    };
    let mut remaining = 0;
    for u in scan(agent_home) {
        let m = &u.marker;
        let title = item_title(m);
        let Some(live) = &live else {
            remaining += usize::from(u.track != TrackState::Absent);
            continue;
        };
        let is_open = live.contains(&title);
        if u.track == TrackState::Absent {
            if is_open && let Err(e) = mur_open_items::resolve(mur_home, &title) {
                tracing::warn!(track_name = %m.track_name, error = %e, "could not resolve open item; marker kept");
                continue;
            }
            mark_closed(agent_home, &m.track_name);
            continue;
        }
        remaining += 1;
        tracing::warn!(
            turn = %m.turn,
            track_name = %m.track_name,
            track = %m.track.display(),
            project = %m.project.display(),
            opened_at = %m.opened_at,
            "unsettled turn track from a previous run; left in place for review"
        );
        if is_open {
            continue; // already listed; don't grow the log every boot
        }
        let next = format!(
            "inspect {} then `git worktree remove` it",
            m.track.display()
        );
        if let Err(e) = mur_open_items::report(mur_home, agent, &title, Some(&next)) {
            tracing::warn!(track_name = %m.track_name, error = %e, "could not raise open item");
        }
    }
    remaining
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(home: &Path, name: &str, track: PathBuf) -> OpenMarker {
        OpenMarker {
            turn: format!("task-{name}"),
            track_name: name.to_string(),
            track,
            project: home.to_path_buf(),
            opened_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn open_then_close_leaves_nothing_to_report() {
        let home = tempfile::tempdir().unwrap();
        let m = marker(home.path(), "turn-a", home.path().join("wt"));
        mark_open(home.path(), &m);
        assert_eq!(scan(home.path()).len(), 1);
        mark_closed(home.path(), "turn-a");
        assert!(scan(home.path()).is_empty());
        mark_closed(home.path(), "turn-a"); // idempotent
    }

    #[test]
    fn boot_reports_live_track_without_touching_it() {
        let home = tempfile::tempdir().unwrap();
        let mur_home = tempfile::tempdir().unwrap();
        let wt = home.path().join("wt-live");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join("edit.txt"), "only copy").unwrap();
        mark_open(home.path(), &marker(home.path(), "turn-live", wt.clone()));
        mark_open(
            home.path(),
            &marker(home.path(), "turn-gone", home.path().join("wt-gone")),
        );

        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 1);
        assert_eq!(
            std::fs::read_to_string(wt.join("edit.txt")).unwrap(),
            "only copy",
            "track is never deleted"
        );
        let items = mur_open_items::open(mur_home.path());
        assert_eq!(items.len(), 1);
        assert!(items[0].title.contains("turn-live"), "{}", items[0].title);
        assert_eq!(
            scan(home.path()).len(),
            1,
            "live marker kept, gone-track marker dropped"
        );

        // Second boot: still open, no duplicate record.
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 1);
        assert_eq!(mur_open_items::open(mur_home.path()).len(), 1);

        // User closes the item but leaves the worktree: it comes back.
        let title = items[0].title.clone();
        mur_open_items::resolve(mur_home.path(), &title).unwrap();
        assert!(mur_open_items::open(mur_home.path()).is_empty());
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 1);
        assert_eq!(mur_open_items::open(mur_home.path()).len(), 1, "re-raised");

        // Worktree removed: item resolves itself, marker goes.
        std::fs::remove_dir_all(&wt).unwrap();
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 0);
        assert!(mur_open_items::open(mur_home.path()).is_empty());
        assert!(scan(home.path()).is_empty());
    }

    #[test]
    fn probe_error_is_unknown_not_absent() {
        let p = Path::new("x");
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(TrackState::from_probe(Err(denied), p), TrackState::Unknown);
        assert_eq!(TrackState::from_probe(Ok(false), p), TrackState::Absent);
        assert_eq!(TrackState::from_probe(Ok(true), p), TrackState::Present);
    }

    /// Review #1913 P2: a worktree behind a revoked grant must not be
    /// mistaken for a deleted one.
    #[cfg(unix)]
    #[test]
    fn inaccessible_worktree_keeps_marker_and_item() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let mur_home = tempfile::tempdir().unwrap();
        let locked = home.path().join("locked");
        let wt = locked.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        mark_open(home.path(), &marker(home.path(), "turn-locked", wt.clone()));
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 1);

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = wt.try_exists().is_err(); // false when running as root
        let n = report_at_boot(home.path(), mur_home.path(), "mur");
        let markers = scan(home.path()).len();
        let items = mur_open_items::open(mur_home.path()).len();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        if denied {
            assert_eq!(n, 1, "unknown still counts as unsettled");
            assert_eq!(markers, 1, "marker kept on access error");
            assert_eq!(items, 1, "open item not resolved on access error");
        }
        assert!(wt.exists(), "worktree untouched");
    }

    /// Review #1913 P2: an unreadable open-item log must not let a marker
    /// go just because "no item looked open".
    #[cfg(unix)]
    #[test]
    fn unreadable_item_log_keeps_marker() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let mur_home = tempfile::tempdir().unwrap();
        let wt = home.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        mark_open(home.path(), &marker(home.path(), "turn-log", wt.clone()));
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 1);
        std::fs::remove_dir_all(&wt).unwrap(); // gone, but item state unknowable

        let log = mur_home.path().join(mur_open_items::LOG_FILE);
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = std::fs::read(&log).is_err(); // false when running as root
        report_at_boot(home.path(), mur_home.path(), "mur");
        let markers = scan(home.path()).len();
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();

        if denied {
            assert_eq!(markers, 1, "marker kept while the log is unreadable");
            assert_eq!(
                mur_open_items::open(mur_home.path()).len(),
                1,
                "item untouched"
            );
        }
        // Log readable again: the absent worktree now settles normally.
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 0);
        assert!(scan(home.path()).is_empty());
        assert!(mur_open_items::open(mur_home.path()).is_empty());
    }

    /// Review #1913 P2: a worktree on an unmounted volume is offline, not
    /// deleted — its project root is missing too.
    #[test]
    fn offline_project_volume_keeps_marker_and_item() {
        let home = tempfile::tempdir().unwrap();
        let mur_home = tempfile::tempdir().unwrap();
        let project = home.path().join("volume").join("proj");
        let wt = project.join(".worktrees").join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let mut m = marker(home.path(), "turn-vol", wt.clone());
        m.project = project.clone();
        mark_open(home.path(), &m);
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 1);

        // "Unmount": the whole volume disappears, project included.
        let vol = home.path().join("volume");
        let parked = home.path().join("parked");
        std::fs::rename(&vol, &parked).unwrap();
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 1);
        assert_eq!(scan(home.path()).len(), 1, "marker kept while offline");
        assert_eq!(mur_open_items::open(mur_home.path()).len(), 1);

        // Remount, then really remove the worktree: settles.
        std::fs::rename(&parked, &vol).unwrap();
        std::fs::remove_dir_all(&wt).unwrap();
        assert_eq!(report_at_boot(home.path(), mur_home.path(), "mur"), 0);
        assert!(scan(home.path()).is_empty());
    }

    #[test]
    fn corrupt_marker_is_skipped_not_fatal() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(OPEN_TRACKS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("turn-bad.json"), b"{ truncated").unwrap();
        std::fs::write(dir.join("turn-x.json.123.tmp"), b"partial").unwrap();
        assert!(scan(home.path()).is_empty());
    }
}
