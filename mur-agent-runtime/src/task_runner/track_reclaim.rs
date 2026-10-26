//! Runtime side of turn-track reclaiming (#2152): release a track the close
//! path leaves behind, and sweep the project's stale tracks when a new one
//! opens. The ownership rules live in `mur_track::turn::reclaim`.

use mur_track::TurnTrack;
use mur_track::turn::reclaim::{reclaim, stale_tracks};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Under `<mur_home>/artifacts/<agent>/`: where reclaimed tracks' patches go.
pub(super) const RECLAIM_ARTIFACT_DIR: &str = "turn-tracks";

/// `<mur_home>/artifacts`, matching the sandbox's artifacts grant.
const ARTIFACTS_DIR: &str = "artifacts";

/// Hand a track the close path could not remove over to the sweep. A
/// failure only means the track waits for its owner to die instead.
pub(super) fn release(turn: &str, track: &TurnTrack) {
    if let Err(e) = track.release() {
        tracing::warn!(turn, track = %track.path().display(), error = %e, "turn track not released; reclaimed only after this runtime exits");
    }
}

/// `<mur_home>/artifacts/<agent>/turn-tracks`, from the agent home
/// `<mur_home>/agents/<agent>` — the exact derivation of the agent's
/// artifacts write grant (`sandbox::policy::build`), so the sealed runtime
/// may write here.
pub(super) fn patch_dir(agent_home: &Path) -> Option<PathBuf> {
    let mur_home = agent_home.parent()?.parent()?;
    let agent = agent_home.file_name()?;
    Some(
        mur_home
            .join(ARTIFACTS_DIR)
            .join(agent)
            .join(RECLAIM_ARTIFACT_DIR),
    )
}

/// The configured grace, `turn_track.stale_grace_mins`.
pub(super) fn grace() -> Duration {
    let mins = mur_common::config::Config::load_or_default(
        &mur_common::trust::mur_home().join("config.yaml"),
    )
    .turn_track
    .stale_grace_mins;
    Duration::from_secs(mins.saturating_mul(60))
}

/// Reclaim every stale track in `project`, saving each diff under
/// `patches` first. A non-empty patch raises an open item on `notify`'s
/// `(mur_home, agent)` so the user learns where the edits went. Returns the
/// patches written. Never fails the turn.
pub(super) fn sweep(
    turn: &str,
    project: &Path,
    patches: &Path,
    grace: Duration,
    notify: Option<(&Path, &str)>,
) -> Vec<PathBuf> {
    let stale = match stale_tracks(project, grace) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(turn, project = %project.display(), error = %e, "stale turn track scan failed");
            return Vec::new();
        }
    };
    let mut saved = Vec::new();
    for t in stale {
        let patch = patches.join(format!("{}.patch", t.name));
        match reclaim(project, &t, &patch) {
            Ok(()) => {
                tracing::warn!(
                    turn,
                    track_name = %t.name,
                    reason = ?t.reason,
                    patch = %patch.display(),
                    "reclaimed stale turn track; its diff is saved as a patch"
                );
                if let Some((mur_home, agent)) = notify {
                    raise_item(mur_home, agent, &t.name, &patch);
                }
                saved.push(patch);
            }
            Err(e) => {
                tracing::warn!(turn, track_name = %t.name, error = %e, "stale turn track not reclaimed")
            }
        }
    }
    saved
}

/// One open item per reclaimed track that held edits; an empty diff is
/// nothing to review.
fn raise_item(mur_home: &Path, agent: &str, track_name: &str, patch: &Path) {
    if std::fs::metadata(patch).map(|m| m.len()).unwrap_or(0) == 0 {
        return;
    }
    let title = format!("Review edits from reclaimed turn track {track_name}");
    let next = format!("git apply --3way {}", patch.display());
    if let Err(e) = mur_open_items::report(mur_home, agent, &title, Some(&next)) {
        tracing::warn!(track_name, error = %e, "could not raise open item for reclaimed track");
    }
}

/// [`sweep`] on its own thread: saving a gutted track's patch walks the
/// whole tree, and the new turn must not wait for it.
pub(super) fn sweep_in_background(turn: &str, project: PathBuf, agent_home: &Path) {
    let Some(patches) = patch_dir(agent_home) else {
        return;
    };
    let mur_home = patches.ancestors().nth(3).map(Path::to_path_buf);
    let agent = agent_home
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let turn = turn.to_string();
    std::thread::spawn(move || {
        let notify = mur_home.as_deref().zip(agent.as_deref());
        sweep(&turn, &project, &patches, grace(), notify);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_track::TreeClone;

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let td = tempfile::tempdir().unwrap();
        let root = mur_track::turn::canonicalize(td.path()).unwrap();
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap();
            assert!(st.success());
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        (td, root)
    }

    #[test]
    fn patch_dir_matches_the_artifacts_grant() {
        assert_eq!(
            patch_dir(Path::new("/h/.mur/agents/mur")).unwrap(),
            Path::new("/h/.mur/artifacts/mur/turn-tracks")
        );
        assert!(patch_dir(Path::new("/")).is_none());
    }

    /// The acceptance case from #2152: a released track is reclaimed with
    /// its edits saved and an open item raised, while a track a live
    /// session still owns on the same project is left alone.
    #[test]
    fn sweep_reclaims_released_and_spares_a_live_session() {
        let (_td, project) = repo();
        let home = tempfile::tempdir().unwrap();
        let kept = TurnTrack::create(&project, "turn-kept", TreeClone::Copy).unwrap();
        std::fs::write(kept.path().join("a.txt"), "edit\n").unwrap();
        release("t1", &kept);
        let live = TurnTrack::create(&project, "turn-live", TreeClone::Copy).unwrap();

        let patches = home.path().join("patches");
        let saved = sweep(
            "t2",
            &project,
            &patches,
            Duration::ZERO,
            Some((home.path(), "mur")),
        );

        assert_eq!(saved, vec![patches.join("turn-kept.patch")]);
        assert!(!kept.path().exists());
        assert!(live.path().exists());
        assert!(
            std::fs::read_to_string(&saved[0])
                .unwrap()
                .contains("+edit")
        );
        let items = mur_open_items::open(home.path());
        assert_eq!(items.len(), 1);
        assert!(items[0].title.contains("turn-kept"));
    }

    #[test]
    fn empty_reclaim_raises_no_item() {
        let (_td, project) = repo();
        let home = tempfile::tempdir().unwrap();
        let t = TurnTrack::create(&project, "turn-clean", TreeClone::Copy).unwrap();
        release("t1", &t);
        let saved = sweep(
            "t2",
            &project,
            &home.path().join("p"),
            Duration::ZERO,
            Some((home.path(), "mur")),
        );
        assert_eq!(saved.len(), 1);
        assert!(!t.path().exists());
        assert!(mur_open_items::open(home.path()).is_empty());
    }
}
