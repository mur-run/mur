//! Refuse a `bash` command that would detach a live turn track.
//!
//! A turn track is a git worktree the runtime owns: it promotes the track's
//! diff back onto the project and destroys it when the turn ends. A model that
//! removes, guts, moves, or unregisters that track underneath the runtime
//! loses the turn's work, and the end-of-turn promote then sees a gutted tree.
//! `TurnTrack::promote` refuses that state too; this guard stops the command
//! before it runs, while the model can still be told why.
//!
//! "Live" comes from the runtime's own state ([`SessionCwd::live_tracks`]),
//! never from directory names, so a track a previous run left unsettled stays
//! removable. Paths are compared by file identity when they exist, so case
//! (APFS), symlinks, and `..` spellings all land on the same track.
//!
//! Quotes and shell expansion are not parsed: `$T`, `eval`, or a script file
//! slip past, and the promote guard is the one that catches them.
//!
//! [`SessionCwd::live_tracks`]: super::fs_policy::SessionCwd::live_tracks

use super::bash_track::{command_segments, is_assignment};
use mur_trash::detect::{REMOVERS, WRAPPERS};
use std::path::{Component, Path, PathBuf};

/// Deletes its path arguments too. The trash interceptor leaves it out on
/// purpose (it is already recoverable), but it still guts a live track.
const GUARD_ONLY_REMOVER: &str = "trash";

/// The trash interceptor's removers plus [`GUARD_ONLY_REMOVER`]; sharing the
/// list keeps the guard and the interceptor agreeing on what deletes.
fn is_remover(prog: &str) -> bool {
    prog == GUARD_ONLY_REMOVER || REMOVERS.contains(&prog)
}

/// Why a command was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    /// The turn that owns the track.
    pub turn: String,
    pub track: PathBuf,
    /// The offending command, as the model spelled it.
    pub action: String,
    verb: &'static str,
}

impl Refusal {
    fn track_name(&self) -> String {
        self.track
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn message(&self) -> String {
        format!(
            "refused: `{}` would {} {}, the live track of turn {}. The runtime \
             promotes and removes it itself when that turn ends; leave it alone, \
             or run the command after the turn settles.",
            self.action,
            self.verb,
            self.track.display(),
            self.turn
        )
    }
}

/// Check `command`, log a refusal, and return the message for the model.
pub(crate) fn guard(command: &str, cwd: &Path, live: &[(String, PathBuf)]) -> Option<String> {
    let r = refuse_track_detach(command, cwd, live)?;
    tracing::warn!(
        turn = %r.turn,
        track_name = %r.track_name(),
        track = %r.track.display(),
        action = %r.action,
        "bash refused: command would detach a live turn track"
    );
    Some(r.message())
}

/// `Some` when `command`, run in `cwd`, would remove, gut, move, or
/// unregister one of `live` (`(turn, track)` pairs open right now).
pub(crate) fn refuse_track_detach(
    command: &str,
    cwd: &Path,
    live: &[(String, PathBuf)],
) -> Option<Refusal> {
    if live.is_empty() {
        return None;
    }
    let live: Vec<(String, PathBuf)> = live.iter().map(|(t, p)| (t.clone(), resolve(p))).collect();
    let mut base = cwd.to_path_buf();
    for seg in command_segments(command) {
        let words = words_of(seg);
        let Some((prog, args)) = words.split_first() else {
            continue;
        };
        if *prog == "cd" {
            if let Some(dir) = args.first() {
                base = resolve(&join(&base, dir));
            }
            continue;
        }
        if let Some(r) = check_segment(prog, args, &base, &live) {
            return Some(Refusal {
                action: seg.trim().to_string(),
                ..r
            });
        }
    }
    None
}

/// The segment's words with env assignments and wrapper prefixes dropped.
fn words_of(seg: &str) -> Vec<&str> {
    let mut words: Vec<&str> = seg.split_whitespace().map(unquote).collect();
    while words
        .first()
        .is_some_and(|w| is_assignment(w) || WRAPPERS.contains(&basename(w)))
    {
        words.remove(0);
    }
    words
}

fn check_segment(
    prog: &str,
    args: &[&str],
    base: &Path,
    live: &[(String, PathBuf)],
) -> Option<Refusal> {
    let refusal = |(turn, track): &(String, PathBuf), verb| Refusal {
        turn: turn.clone(),
        track: track.clone(),
        action: String::new(),
        verb,
    };
    match basename(prog) {
        "git" => git_segment(args, base, live).map(|(hit, verb)| refusal(hit, verb)),
        p if is_remover(p) => path_args(args)
            .iter()
            .find_map(|a| contains_live(&join(base, a), live))
            .map(|hit| refusal(hit, "remove or gut")),
        "mv" => {
            let paths = path_args(args);
            let (_, sources) = paths.split_last()?;
            sources
                .iter()
                .find_map(|a| contains_live(&join(base, a), live))
                .map(|hit| refusal(hit, "move"))
        }
        _ => None,
    }
}

/// `git [globals] worktree remove|move|prune …` that hits a live track.
fn git_segment<'a>(
    args: &[&str],
    base: &Path,
    live: &'a [(String, PathBuf)],
) -> Option<(&'a (String, PathBuf), &'static str)> {
    let mut words = args.iter().copied();
    let mut base = base.to_path_buf();
    // Global options up to the subcommand; `-C` moves the base.
    loop {
        match words.next()? {
            "-C" => base = join(&base, words.next()?),
            "-c" => {
                words.next()?;
            }
            "worktree" => break,
            w if w.starts_with('-') => {}
            _ => return None,
        }
    }
    let op = words.next()?;
    let rest: Vec<&str> = words.collect();
    match op {
        "remove" | "move" => {
            let target = join(&base, rest.iter().find(|w| !w.starts_with('-'))?);
            let verb = if op == "remove" { "remove" } else { "move" };
            live.iter()
                .find(|(_, t)| same_file(&target, t))
                .map(|hit| (hit, verb))
        }
        // Prune drops the registration of any worktree whose directory is
        // gone; a live track in that state is still the turn's to settle.
        "prune" if !rest.iter().any(|w| matches!(*w, "-n" | "--dry-run")) => live
            .iter()
            .find(|(_, t)| !t.join(".git").exists())
            .map(|hit| (hit, "drop the registration of")),
        _ => None,
    }
}

/// Path arguments: everything after `--`, else every non-flag word.
fn path_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    match args.iter().position(|w| *w == "--") {
        Some(i) => {
            let mut out: Vec<&str> = args[..i]
                .iter()
                .copied()
                .filter(|w| !w.starts_with('-'))
                .collect();
            out.extend(&args[i + 1..]);
            out
        }
        None => args
            .iter()
            .copied()
            .filter(|w| !w.starts_with('-'))
            .collect(),
    }
}

/// The live track that `target` is, or is an ancestor of.
fn contains_live<'a>(
    target: &Path,
    live: &'a [(String, PathBuf)],
) -> Option<&'a (String, PathBuf)> {
    live.iter()
        .find(|(_, t)| t.ancestors().any(|a| same_file(target, a)))
}

/// `base`-relative `word`, with `~` expanded and a trailing `/*` (gutting a
/// directory's contents) folded into the directory itself.
fn join(base: &Path, word: &str) -> PathBuf {
    let word = word
        .strip_suffix("/*")
        .or_else(|| word.strip_suffix("/.*"))
        .unwrap_or(word);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (word, home) {
        ("~", Some(h)) => h,
        (w, Some(h)) if w.starts_with("~/") => h.join(&w[2..]),
        (w, _) => base.join(w),
    }
}

/// Same file by identity when both exist (case, symlinks, `..`), else by
/// normalized path.
fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return ma.dev() == mb.dev() && ma.ino() == mb.ino();
        }
    }
    resolve(a) == resolve(b)
}

fn basename(w: &str) -> &str {
    w.rsplit('/').next().unwrap_or(w)
}

fn unquote(w: &str) -> &str {
    w.trim_matches(|c| c == '\'' || c == '"')
}

/// Canonical when the path exists, else lexically normalized.
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
