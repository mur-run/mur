//! Per-turn tracks (design spec §4.1–4.2): the model works in a git worktree
//! whose tree is a clone of the project, and the turn's edits are promoted
//! back at the end. Decided *before* the model sees the prompt, from the
//! tool registry alone — never from the user message or the first tool
//! call, because the model is handed an absolute working directory and uses
//! absolute paths inside bash, so a late rebind would leave a hole.
//!
//! The model sees track paths everywhere — prompt, tool results — and only
//! the settlement card speaks in project-relative paths. Rewriting tool
//! results to project paths was considered and rejected: a model that reads
//! `/project/src/a.rs` in a result starts using it in bash, and that path
//! bypasses the track.

use super::*;
use mur_track::turn::target::{remove_turn_target, sweep_orphan_turn_targets, turn_target_dir};
use mur_track::{TreeClone, TurnTrack, UndoStore};
use std::path::{Path, PathBuf};

/// Tools whose presence makes a turn write-capable. `bash` is here because
/// a shell can write anything; the file tools because that is their job.
pub(super) const WRITE_CAPABLE_TOOLS: &[&str] = &["bash", "write_file", "edit_file"];

/// Opt-out for operators that cannot afford the per-turn clone (or are
/// debugging it). Any non-empty value other than `0` disables tracks.
pub(super) const TURN_TRACK_ENV: &str = "MUR_TURN_TRACK";

/// What one turn is working inside.
pub(super) struct OpenTrack {
    pub track: TurnTrack,
    /// The directory the turn had before it was redirected into the track,
    /// so the conversation's cwd can be restored for the next turn.
    pub original_cwd: PathBuf,
}

impl TaskRunner {
    /// Does this runner's tool set make every turn write-capable?
    pub(super) fn turn_is_write_capable(&self) -> bool {
        self.tools
            .iter()
            .any(|t| WRITE_CAPABLE_TOOLS.contains(&t.name()))
    }

    fn turn_tracks_enabled() -> bool {
        match std::env::var(TURN_TRACK_ENV) {
            Ok(v) => v.is_empty() || v == "0" || v.eq_ignore_ascii_case("off"),
            Err(_) => true,
        }
    }

    /// Where the track for `turn` would be created, or `None` when the
    /// turn gets no track: tracks are off, no write-capable tool is bound,
    /// or the runner has no session cwd.
    fn track_origin(&self, turn: &str) -> Option<PathBuf> {
        if !Self::turn_tracks_enabled() || !self.turn_is_write_capable() {
            return None;
        }
        let (session, _) = self.session_cwd.as_ref()?;
        Some(session.for_turn(turn))
    }

    /// Create the track on disk. Only a repository root gets one: the tree
    /// clone and the diff are both scoped to it, and a cwd inside a repo but
    /// not at its root is left alone rather than guessed at. Failure is
    /// logged, never raised — a turn must not fail because its safety net
    /// did.
    fn create_track(turn: &str, origin: &Path) -> Option<TurnTrack> {
        match TurnTrack::create(origin, &track_name(turn), TreeClone::detect()) {
            Ok(t) => {
                sweep_turn_targets(turn, t.project());
                Some(t)
            }
            Err(e) => {
                tracing::warn!(turn, cwd = %origin.display(), error = %e, "no turn track; working on the project directly");
                None
            }
        }
    }

    fn park_track(&self, turn: &str, track: TurnTrack, original_cwd: PathBuf) {
        if let Some((session, _)) = self.session_cwd.as_ref() {
            session.enter_track(
                turn,
                track.project().to_path_buf(),
                track.path().to_path_buf(),
            );
        }
        tracing::info!(
            turn,
            track_name = %track_dir_name(&track).unwrap_or_default(),
            track = %track.path().display(),
            "turn track open"
        );
        if let (Some((session, _)), Some(name)) =
            (self.session_cwd.as_ref(), track_dir_name(&track))
        {
            super::unsettled::mark_open(
                session.home(),
                &super::unsettled::OpenMarker {
                    turn: turn.to_string(),
                    track_name: name,
                    track: track.path().to_path_buf(),
                    project: track.project().to_path_buf(),
                    opened_at: chrono::Utc::now(),
                },
            );
        }
        self.turn_tracks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                turn.to_string(),
                OpenTrack {
                    track,
                    original_cwd,
                },
            );
    }

    /// Create the turn's track and redirect the turn's cwd into it — on a
    /// blocking thread, since the clone walks the whole project tree.
    pub(super) async fn begin_turn_track(&self, turn: &str) {
        let Some(origin) = self.track_origin(turn) else {
            return;
        };
        let id = turn.to_string();
        let created = tokio::task::spawn_blocking({
            let origin = origin.clone();
            move || Self::create_track(&id, &origin)
        })
        .await
        .ok()
        .flatten();
        if let Some(track) = created {
            self.park_track(turn, track, origin);
        }
    }

    /// Synchronous twin of [`Self::begin_turn_track`] for tests.
    #[cfg(test)]
    pub(super) fn open_turn_track(&self, turn: &str) -> Option<OpenTrack> {
        let origin = self.track_origin(turn)?;
        let track = Self::create_track(turn, &origin)?;
        self.park_track(turn, track, origin.clone());
        self.take_turn_track(turn)
    }

    pub(super) fn take_turn_track(&self, turn: &str) -> Option<OpenTrack> {
        self.turn_tracks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(turn)
    }

    /// `settle`, after the turn's track (if any) has been promoted and its
    /// diff written into the ledger — so the card's `~ changed` is what the
    /// project actually received, not what the tools said they did.
    pub(super) async fn settle_turn(
        &self,
        turn: &str,
        text: String,
        ledger: &crate::turn_ledger::TurnLedger,
    ) -> Message {
        match self.take_turn_track(turn) {
            Some(open) => {
                let mut ledger = ledger.clone();
                self.close_turn_track_blocking(turn, open, &mut ledger)
                    .await;
                settle(text, &ledger)
            }
            None => settle(text, ledger),
        }
    }

    /// A turn that ended without settling (cancelled, LLM error, panic in
    /// the loop) still owns a track. Promote and destroy it exactly as a
    /// settled turn would: edits already made are kept, matching what the
    /// same turn did before tracks existed, when they landed directly.
    pub(super) async fn sweep_turn_track(&self, turn: &str) {
        if let Some(open) = self.take_turn_track(turn) {
            let mut discard = crate::turn_ledger::TurnLedger::default();
            self.close_turn_track_blocking(turn, open, &mut discard)
                .await;
            if let Some(kept) = discard.track_kept {
                tracing::error!(turn, track = %kept, "unsettled turn: promote failed, track kept");
            }
        }
    }

    /// [`Self::close_turn_track`] with the disk work on a blocking thread.
    async fn close_turn_track_blocking(
        &self,
        turn: &str,
        open: OpenTrack,
        ledger: &mut crate::turn_ledger::TurnLedger,
    ) {
        if let Some((session, _)) = self.session_cwd.as_ref() {
            session.leave_track(turn, open.original_cwd.clone());
        }
        let id = turn.to_string();
        let undo = self.undo_store();
        let marker = self.marker_of(&open.track);
        let closed = tokio::task::spawn_blocking(move || {
            let mut l = crate::turn_ledger::TurnLedger::default();
            close_track(&id, open.track, undo.as_ref(), &mut l);
            l
        })
        .await;
        match closed {
            Ok(l) => {
                ledger.files_changed = l.files_changed;
                ledger.track_kept = l.track_kept;
            }
            Err(e) => tracing::error!(turn, error = %e, "turn track close panicked"),
        }
        if let Some((home, name)) = marker {
            super::unsettled::mark_closed(&home, &name);
        }
    }

    /// `(agent home, track dir name)` for this runner's open-track marker.
    fn marker_of(&self, track: &TurnTrack) -> Option<(PathBuf, String)> {
        let (session, _) = self.session_cwd.as_ref()?;
        Some((session.home().to_path_buf(), track_dir_name(track)?))
    }

    /// Report tracks a previous process opened and never closed (#1909).
    /// Call once at boot, before the first turn.
    pub fn report_unsettled_tracks(&self, mur_home: &Path) -> usize {
        let Some((session, _)) = self.session_cwd.as_ref() else {
            return 0;
        };
        super::unsettled::report_at_boot(session.home(), mur_home, &self.agent_name)
    }

    /// End the turn synchronously: restore the cwd, promote, record,
    /// destroy. Tests use this; the loop goes through the blocking variant.
    #[cfg(test)]
    pub(super) fn close_turn_track(
        &self,
        turn: &str,
        open: OpenTrack,
        ledger: &mut crate::turn_ledger::TurnLedger,
    ) -> TrackClose {
        if let Some((session, _)) = self.session_cwd.as_ref() {
            session.leave_track(turn, open.original_cwd.clone());
        }
        let marker = self.marker_of(&open.track);
        let outcome = close_track(turn, open.track, self.undo_store().as_ref(), ledger);
        if let Some((home, name)) = marker {
            super::unsettled::mark_closed(&home, &name);
        }
        outcome
    }

    /// The per-agent undo store (spec §4.1 step 4), rooted at the session
    /// home — the agent dir, where `profile.yaml` lives. `None` on runners
    /// without a session cwd (tests, stubs): their turns have no track
    /// anyway.
    fn undo_store(&self) -> Option<UndoStore> {
        self.session_cwd
            .as_ref()
            .map(|(session, _)| UndoStore::new(session.home()))
    }
}

/// Snapshot the before-bytes, promote the track to the project, record what
/// moved (project-relative) in the ledger, destroy the track. A failed
/// promote keeps the track on disk and says so in the ledger rather than
/// losing work; a failed destroy after a good promote is only logged — the
/// edits are already home. A failed snapshot is logged and the promote goes
/// ahead: the turn's edits are the deliverable, the undo is the safety net,
/// and a net that blocks the deliverable would get switched off.
fn close_track(
    turn: &str,
    track: TurnTrack,
    undo: Option<&UndoStore>,
    ledger: &mut crate::turn_ledger::TurnLedger,
) -> TrackClose {
    let name = track_dir_name(&track).unwrap_or_default();
    let path = track.path().display().to_string();
    if let Some(store) = undo {
        match track.diff_files() {
            Ok(files) => {
                if let Err(e) = store.snapshot(turn, track.project(), track.path(), &files) {
                    tracing::warn!(turn, error = %e, "undo snapshot failed; promoting without it");
                }
            }
            Err(e) => tracing::warn!(turn, error = %e, "undo snapshot skipped: diff failed"),
        }
    }
    let (outcome, files) = match track.promote() {
        Ok(files) => {
            let count = files.len();
            ledger.files_changed = Some(
                files
                    .iter()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect(),
            );
            let turn_dir = turn_target_dir_of(&track);
            if let Err(e) = track.destroy() {
                tracing::warn!(turn, error = %e, "turn track promoted but not destroyed");
                (TrackClose::PromotedNotDestroyed, count)
            } else {
                if let Some(dir) = turn_dir {
                    remove_turn_target_in_background(turn, dir);
                }
                (TrackClose::Promoted, count)
            }
        }
        Err(e) => {
            tracing::error!(turn, track = %track.path().display(), error = %e, "promote failed; track kept for review");
            ledger.track_kept = Some(track.path().to_string_lossy().into_owned());
            (TrackClose::Kept, 0)
        }
    };
    // One line per close, success included: a `turn track open` with no
    // matching close means the runtime died before closing (#1909).
    tracing::info!(
        turn,
        track_name = %name,
        track = %path,
        outcome = outcome.as_str(),
        files,
        "turn track closed"
    );
    outcome
}

/// How a turn track was closed; `as_str` is the `outcome` log field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TrackClose {
    /// Promoted and the worktree removed.
    Promoted,
    /// Promoted, but the worktree is still on disk.
    PromotedNotDestroyed,
    /// Promote failed; the worktree is kept for review.
    Kept,
}

impl TrackClose {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Promoted => "promoted",
            Self::PromotedNotDestroyed => "promoted_not_destroyed",
            Self::Kept => "kept",
        }
    }
}

/// The track's directory name — what `git worktree list` shows, so an
/// operator can grep the log for a leftover worktree by its dir name.
/// `turn_track.warm_build_cache.turn_target_subdir`, validated.
fn turn_target_subdir() -> String {
    mur_common::config::Config::load_or_default(&mur_common::trust::mur_home().join("config.yaml"))
        .turn_track
        .warm_build_cache
        .validated_subdir()
        .to_string()
}

/// Remove turn dirs whose worktree is gone (a crashed or killed turn, here or
/// in another agent on the same project). Never fails the turn.
fn sweep_turn_targets(turn: &str, project: &Path) {
    let report = sweep_orphan_turn_targets(project, &turn_target_subdir());
    if !report.removed.is_empty() {
        tracing::info!(
            turn,
            removed = report.removed.len(),
            "swept orphan turn target dirs"
        );
    }
    for (path, error) in report.failed {
        tracing::warn!(turn, path = %path.display(), %error, "orphan turn target dir not removed");
    }
}

/// The track's directory name — what `git worktree list` shows.
fn track_dir_name(track: &TurnTrack) -> Option<String> {
    Some(track.path().file_name()?.to_str()?.to_string())
}

/// `<project>/target/<subdir>/<track-name>` for this track.
fn turn_target_dir_of(track: &TurnTrack) -> Option<PathBuf> {
    let name = track.path().file_name()?.to_str()?;
    Some(turn_target_dir(
        track.project(),
        &turn_target_subdir(),
        name,
    ))
}

/// Delete the destroyed track's turn dir off this thread: a warm target is
/// many GiB and the turn must not wait for it. A failure is left to the
/// next sweep.
fn remove_turn_target_in_background(turn: &str, dir: PathBuf) {
    let turn = turn.to_string();
    std::thread::spawn(move || {
        if let Err(e) = remove_turn_target(&dir) {
            tracing::warn!(turn, dir = %dir.display(), error = %e, "turn target dir not removed; next sweep retries");
        }
    });
}

/// Track directory name for a turn id. Task ids are `task-<uuid>` or
/// caller-supplied; squeeze to the fleet-name alphabet the track layer
/// enforces, keeping the tail where the uuid's entropy lives.
pub(super) fn track_name(turn: &str) -> String {
    let cleaned: String = turn
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' | '_' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect();
    const MAX: usize = 48;
    let tail = if cleaned.len() > MAX {
        &cleaned[cleaned.len() - MAX..]
    } else {
        &cleaned
    };
    format!("turn-{}", tail.trim_matches('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_name_is_fleet_safe_and_keeps_the_tail() {
        let n = track_name("task-0192ABCD-ef01-7000-8000-0123456789ab");
        assert!(mur_common::fleet::valid_fleet_name(&n), "{n}");
        assert!(n.ends_with("0123456789ab"), "{n}");
        assert!(n.starts_with("turn-"));
        let long = track_name(&"x".repeat(200));
        assert!(long.len() <= 5 + 48);
        assert!(mur_common::fleet::valid_fleet_name(&long));
    }

    #[test]
    fn write_capability_is_decided_from_the_registry() {
        let r = TaskRunner::new_stub_echo();
        assert!(!r.turn_is_write_capable(), "no tools → no track");
        let r = TaskRunner::new_stub_echo().with_tools(vec![Arc::new(
            crate::tools::read_file::ReadFileTool::new_for_test(
                crate::tools::fs_policy::SessionCwd::new("/tmp".into()),
                mur_common::agent::FilesystemEntitlement::default(),
            ),
        )]);
        assert!(!r.turn_is_write_capable(), "read-only tool → no track");
        let r = TaskRunner::new_stub_echo().with_tools(vec![Arc::new(
            crate::tools::write_file::WriteFileTool::new_for_test(
                crate::tools::fs_policy::SessionCwd::new("/tmp".into()),
                mur_common::agent::FilesystemEntitlement::default(),
            ),
        )]);
        assert!(r.turn_is_write_capable());
    }

    /// The whole P0 loop on a stub runner: a write-capable tool is bound, so
    /// the turn gets a track at a repo root, its cwd is the track, and the
    /// edit lands in the project with the project-relative path in the ledger.
    #[test]
    fn open_edit_close_promotes_and_records_project_paths() {
        let td = tempfile::tempdir().unwrap();
        let project = mur_track::turn::canonicalize(td.path()).unwrap();
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap();
            assert!(st.success());
        };
        git(&["init", "-q"]);
        std::fs::write(project.join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);

        let cwd = crate::tools::fs_policy::SessionCwd::new(project.clone());
        let runner = TaskRunner::new_stub_echo()
            .with_tools(vec![Arc::new(
                crate::tools::write_file::WriteFileTool::new_for_test(
                    cwd.clone(),
                    mur_common::agent::FilesystemEntitlement::default(),
                ),
            )])
            .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);
        cwd.begin_turn("t1", None, Some(project.clone()));

        let open = runner.open_turn_track("t1").expect("track at a repo root");
        let track_dir = open.track.path().to_path_buf();
        assert_eq!(cwd.for_turn("t1"), track_dir, "turn cwd redirected");
        assert!(track_dir.starts_with(project.join(".worktrees")));
        let markers = project.join(super::super::unsettled::OPEN_TRACKS_DIR);
        assert_eq!(
            std::fs::read_dir(&markers).unwrap().count(),
            1,
            "open track recorded"
        );

        std::fs::write(track_dir.join("a.txt"), "edited").unwrap();
        let mut ledger = crate::turn_ledger::TurnLedger::default();
        let outcome = runner.close_turn_track("t1", open, &mut ledger);
        assert_eq!(outcome, TrackClose::Promoted);
        assert_eq!(
            std::fs::read_dir(&markers).unwrap().count(),
            0,
            "closed track clears its marker"
        );

        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "edited"
        );
        assert_eq!(
            ledger.files_changed.as_deref(),
            Some(&["a.txt".to_string()][..])
        );
        assert_eq!(cwd.for_turn("t1"), project, "cwd restored");
        assert!(!track_dir.exists(), "track destroyed");
        assert!(ledger.track_kept.is_none());

        // §4.1 step 4: the promote left an undo snapshot in the agent home
        // (the session home), and undoing it restores the before bytes.
        let store = UndoStore::new(&project);
        let mut m = store.load("t1").unwrap().expect("manifest written");
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].kind, mur_track::EntryKind::Modified);
        store.undo(&mut m).unwrap();
        assert_eq!(std::fs::read_to_string(project.join("a.txt")).unwrap(), "a");
    }

    /// The undo is the safety net, the edits are the deliverable: when the
    /// snapshot cannot be written (here a plain file squats on the store's
    /// `edits/` dir), the promote still lands and the track is still
    /// destroyed — only the undo manifest is missing.
    #[test]
    fn failed_snapshot_does_not_block_promote() {
        let td = tempfile::tempdir().unwrap();
        let project = mur_track::turn::canonicalize(td.path()).unwrap();
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap();
            assert!(st.success());
        };
        git(&["init", "-q"]);
        std::fs::write(project.join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        // Untracked, so the track (cut from HEAD) never sees it; it only
        // blocks `create_dir_all(<home>/edits/cas)`.
        std::fs::write(project.join(mur_track::undo::EDITS_DIR), "squat").unwrap();

        let cwd = crate::tools::fs_policy::SessionCwd::new(project.clone());
        let runner = TaskRunner::new_stub_echo()
            .with_tools(vec![Arc::new(
                crate::tools::write_file::WriteFileTool::new_for_test(
                    cwd.clone(),
                    mur_common::agent::FilesystemEntitlement::default(),
                ),
            )])
            .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);
        cwd.begin_turn("t1", None, Some(project.clone()));
        let open = runner.open_turn_track("t1").expect("track at a repo root");
        let track_dir = open.track.path().to_path_buf();
        std::fs::write(track_dir.join("a.txt"), "edited").unwrap();
        let mut ledger = crate::turn_ledger::TurnLedger::default();
        let outcome = runner.close_turn_track("t1", open, &mut ledger);
        assert_eq!(outcome, TrackClose::Promoted);

        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "edited",
            "promote landed despite the failed snapshot"
        );
        assert_eq!(
            ledger.files_changed.as_deref(),
            Some(&["a.txt".to_string()][..])
        );
        assert!(ledger.track_kept.is_none());
        assert!(!track_dir.exists(), "track destroyed");
        assert!(
            project.join(mur_track::undo::EDITS_DIR).is_file(),
            "the squatter is untouched: no store, no manifest"
        );
    }

    /// #1909 asks for the success-path close line itself, not just the enum:
    /// exactly one `turn track closed` event per close, carrying the turn,
    /// the track name, the outcome label and the promoted file count.
    #[test]
    fn successful_close_emits_one_close_log_line() {
        use std::sync::{Arc as SArc, Mutex};
        #[derive(Clone, Default)]
        struct Buf(SArc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // `k=v` or `k="v"`: fmt quotes `&str` fields but not `%Display` ones.
        fn has(line: &str, k: &str, v: &str) -> bool {
            line.contains(&format!("{k}={v}")) || line.contains(&format!("{k}=\"{v}\""))
        }

        let td = tempfile::tempdir().unwrap();
        let project = mur_track::turn::canonicalize(td.path()).unwrap();
        let git = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .args(["-c", "user.email=t@t", "-c", "user.name=t"])
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap();
            assert!(st.success());
        };
        git(&["init", "-q"]);
        std::fs::write(project.join("a.txt"), "a").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);

        let cwd = crate::tools::fs_policy::SessionCwd::new(project.clone());
        let runner = TaskRunner::new_stub_echo()
            .with_tools(vec![Arc::new(
                crate::tools::write_file::WriteFileTool::new_for_test(
                    cwd.clone(),
                    mur_common::agent::FilesystemEntitlement::default(),
                ),
            )])
            .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);
        cwd.begin_turn("t1", None, Some(project.clone()));
        let open = runner.open_turn_track("t1").expect("track at a repo root");
        let name = track_dir_name(&open.track).expect("track dir name");
        std::fs::write(open.track.path().join("a.txt"), "edited").unwrap();

        let buf = Buf::default();
        let w = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_writer(move || w.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let mut ledger = crate::turn_ledger::TurnLedger::default();
        let outcome = tracing::subscriber::with_default(sub, || {
            runner.close_turn_track("t1", open, &mut ledger)
        });
        assert_eq!(outcome, TrackClose::Promoted);

        let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        let closes: Vec<&str> = log
            .lines()
            .filter(|l| l.contains("turn track closed"))
            .collect();
        assert_eq!(closes.len(), 1, "exactly one close line; log: {log}");
        let line = closes[0];
        assert!(has(line, "turn", "t1"), "{line}");
        assert!(has(line, "track_name", &name), "{line}");
        assert!(
            has(line, "outcome", TrackClose::Promoted.as_str()),
            "{line}"
        );
        assert!(has(line, "files", "1"), "{line}");
    }

    /// The close log's `outcome` field is grepped by operators; the labels
    /// are a contract, not prose.
    #[test]
    fn close_outcome_labels_are_stable() {
        assert_eq!(TrackClose::Promoted.as_str(), "promoted");
        assert_eq!(
            TrackClose::PromotedNotDestroyed.as_str(),
            "promoted_not_destroyed"
        );
        assert_eq!(TrackClose::Kept.as_str(), "kept");
    }

    #[test]
    fn no_track_outside_a_repo_root() {
        let td = tempfile::tempdir().unwrap();
        let dir = mur_track::turn::canonicalize(td.path()).unwrap();
        let cwd = crate::tools::fs_policy::SessionCwd::new(dir.clone());
        let runner = TaskRunner::new_stub_echo()
            .with_tools(vec![Arc::new(
                crate::tools::write_file::WriteFileTool::new_for_test(
                    cwd.clone(),
                    mur_common::agent::FilesystemEntitlement::default(),
                ),
            )])
            .with_session_cwd(cwd.clone(), vec![dir.to_string_lossy().into_owned()]);
        cwd.begin_turn("t1", None, Some(dir.clone()));
        assert!(runner.open_turn_track("t1").is_none());
        assert_eq!(cwd.for_turn("t1"), dir, "cwd untouched");
    }
}
