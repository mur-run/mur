//! The "these are base symbols" note (code-nav option B).
//!
//! A serena entry stays pinned to its fixed `project:` for the agent's life
//! (see the module doc on [`super`]); it is never respawned into a turn's
//! worktree, because each new worktree would cost a full rust-analyzer
//! re-index. The price is that a turn working inside a track gets answers
//! about the base checkout, not its own edits. This note says so on every
//! result, so the model reads changed files itself instead of trusting a
//! stale symbol table.

use std::path::Path;

use mur_track::turn::strip_verbatim;

/// The note to append to a serena result, or `None` when the answer is
/// already about the directory the turn works in.
///
/// `track` is the turn's `(project, track)` from
/// `SessionCwd::track_of`. The note applies only when that track was cut
/// from the same repository the entry serves — either path containing the
/// other, since an entry may point at a subdirectory of the repo root.
///
/// Both sides are compared without the Windows `\\?\` verbatim prefix:
/// code-nav setup stores `std::fs::canonicalize` output (verbatim), while
/// tracks come from `mur_track::turn::canonicalize` (prefix stripped), and
/// `starts_with` treats the two prefixes as different components.
pub fn base_note(entry_project: &Path, track: Option<(&Path, &Path)>) -> Option<String> {
    let (track_project, track_dir) = track?;
    let entry = strip_verbatim(entry_project.to_path_buf());
    let project = strip_verbatim(track_project.to_path_buf());
    let same_repo = entry.starts_with(&project) || project.starts_with(&entry);
    if !same_repo {
        return None;
    }
    Some(format!(
        "[code-nav] These symbols come from the base checkout `{}`, not this turn's worktree `{}`. \
         Edits made in this turn are NOT reflected; read changed files with read_file under the \
         worktree before relying on line numbers or signatures.",
        entry_project.display(),
        track_dir.display(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_track_means_no_note() {
        assert!(base_note(Path::new("/w/repo"), None).is_none());
    }

    #[test]
    fn track_of_the_same_repo_gets_the_note() {
        let note = base_note(
            Path::new("/w/repo"),
            Some((Path::new("/w/repo"), Path::new("/w/repo/.worktrees/turn-1"))),
        )
        .expect("note");
        assert!(note.contains("/w/repo/.worktrees/turn-1"));
        assert!(note.contains("NOT reflected"));
    }

    #[test]
    fn entry_on_a_subdirectory_still_matches() {
        assert!(
            base_note(
                Path::new("/w/repo/app"),
                Some((Path::new("/w/repo"), Path::new("/w/t")))
            )
            .is_some()
        );
    }

    /// Regression: a verbatim entry path (`std::fs::canonicalize`) must match
    /// its prefix-free track project (`mur_track::turn::canonicalize`).
    #[cfg(windows)]
    #[test]
    fn verbatim_entry_matches_plain_track_project() {
        let note = base_note(
            Path::new(r"\\?\C:\repo"),
            Some((
                Path::new(r"C:\repo"),
                Path::new(r"C:\repo\.worktrees\turn-1"),
            )),
        );
        assert!(note.is_some());
        let note = base_note(
            Path::new(r"C:\repo\app"),
            Some((Path::new(r"\\?\C:\repo"), Path::new(r"C:\t"))),
        );
        assert!(note.is_some());
    }

    #[test]
    fn track_of_another_repo_is_left_alone() {
        assert!(
            base_note(
                Path::new("/w/other"),
                Some((Path::new("/w/repo"), Path::new("/w/t")))
            )
            .is_none()
        );
    }
}
