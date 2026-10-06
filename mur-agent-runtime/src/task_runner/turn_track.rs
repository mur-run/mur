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
use mur_track::{TreeClone, TurnTrack};
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
            Ok(t) => Some(t),
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
        tracing::info!(turn, track = %track.path().display(), "turn track open");
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
        let closed = tokio::task::spawn_blocking(move || {
            let mut l = crate::turn_ledger::TurnLedger::default();
            close_track(&id, open.track, &mut l);
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
    }

    /// End the turn synchronously: restore the cwd, promote, record,
    /// destroy. Tests use this; the loop goes through the blocking variant.
    #[cfg(test)]
    pub(super) fn close_turn_track(
        &self,
        turn: &str,
        open: OpenTrack,
        ledger: &mut crate::turn_ledger::TurnLedger,
    ) {
        if let Some((session, _)) = self.session_cwd.as_ref() {
            session.leave_track(turn, open.original_cwd.clone());
        }
        close_track(turn, open.track, ledger);
    }
}

/// Promote the track to the project, record what moved (project-relative)
/// in the ledger, destroy the track. A failed promote keeps the track on
/// disk and says so in the ledger rather than losing work; a failed destroy
/// after a good promote is only logged — the edits are already home.
fn close_track(turn: &str, track: TurnTrack, ledger: &mut crate::turn_ledger::TurnLedger) {
    match track.promote() {
        Ok(files) => {
            ledger.files_changed = Some(
                files
                    .iter()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect(),
            );
            if let Err(e) = track.destroy() {
                tracing::warn!(turn, error = %e, "turn track promoted but not destroyed");
            }
        }
        Err(e) => {
            tracing::error!(turn, track = %track.path().display(), error = %e, "promote failed; track kept for review");
            ledger.track_kept = Some(track.path().to_string_lossy().into_owned());
        }
    }
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

        std::fs::write(track_dir.join("a.txt"), "edited").unwrap();
        let mut ledger = crate::turn_ledger::TurnLedger::default();
        runner.close_turn_track("t1", open, &mut ledger);

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
