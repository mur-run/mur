//! Refuse a `bash` command that would detach a live turn track.
//!
//! A turn track is a git worktree the runtime owns: it promotes the track's
//! diff back onto the project and destroys it when the turn ends. A model that
//! runs `git worktree remove` (or `move`) on that track empties or relocates
//! it underneath the runtime, and the end-of-turn promote then sees a gutted
//! tree. `TurnTrack::promote` refuses that state too; this guard stops the
//! command before it runs, while the model can still be told why.
//!
//! Quotes and shell expansion are not parsed: a miss only means the promote
//! guard is the one that catches it.

use super::bash_track::{command_segments, is_assignment};
use std::path::{Component, Path, PathBuf};

/// `git worktree` subcommands that detach the worktree they name.
const DETACHING: &[&str] = &["remove", "move"];

/// `Some(reason)` when `command`, run in `cwd`, would remove or move one of
/// `live` (the tracks some turn is inside right now).
pub(crate) fn refuse_track_detach(command: &str, cwd: &Path, live: &[PathBuf]) -> Option<String> {
    if live.is_empty() {
        return None;
    }
    let live: Vec<PathBuf> = live.iter().map(|t| resolve(t)).collect();
    command_segments(command).into_iter().find_map(|seg| {
        let (sub, target) = worktree_target(seg, cwd)?;
        let target = resolve(&target);
        live.iter().find(|t| **t == target).map(|t| {
            format!(
                "refused: `git worktree {sub}` would detach {}, the live turn track \
                 MUR is working in. The runtime promotes and removes it itself when \
                 the turn ends; run the command outside the track's lifetime instead.",
                t.display()
            )
        })
    })
}

/// `(subcommand, absolute target)` when `seg` is `git … worktree remove|move <path>`.
fn worktree_target(seg: &str, cwd: &Path) -> Option<(&'static str, PathBuf)> {
    let mut words = seg
        .split_whitespace()
        .map(unquote)
        .skip_while(|w| is_assignment(w));
    let prog = words.next()?;
    if prog.rsplit('/').next() != Some("git") {
        return None;
    }
    let mut base = cwd.to_path_buf();
    // Global options up to the subcommand; `-C` moves the base.
    loop {
        match words.next()? {
            "-C" => base = base.join(words.next()?),
            "-c" => {
                words.next()?;
            }
            "worktree" => break,
            w if w.starts_with('-') => {}
            _ => return None,
        }
    }
    let sub = words.next()?;
    let sub = DETACHING.iter().copied().find(|d| *d == sub)?;
    let path = words.find(|w| !w.starts_with('-'))?;
    Some((sub, base.join(path)))
}

fn unquote(w: &str) -> &str {
    w.trim_matches(|c| c == '\'' || c == '"')
}

/// Canonical when the path exists, else lexically normalized, so `./x/..`
/// spellings and symlinked temp roots still compare equal.
fn resolve(p: &Path) -> PathBuf {
    if let Ok(c) = mur_track::turn::canonicalize(p) {
        return c;
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
#[path = "bash_guard_tests.rs"]
mod tests;
