//! Classifying a kernel denial that reached us through a child's stderr.
//!
//! The sandbox is sealed in-kernel at process start, so a denial is not an
//! error MUR raises — it is an errno some tool we spawned printed and gave up
//! on. Without a translation the model sees `Operation not permitted` and hands
//! the command back to the user, which is the one outcome this exists to avoid.
//!
//! Only the *write* half lives here so far. `spawn_denied_path` /
//! `spawn_denied_hint` in `bash.rs` are the same concern and belong here too;
//! moving them is pure code motion and is deliberately left to its own change,
//! so this one stays reviewable as a behaviour change.

use std::path::{Path, PathBuf};

/// Extract the path from a kernel *write* denial in `stderr`.
///
/// Sibling of [`spawn_denied_path`], which handles the exec case. That one can
/// key on exit 126 and an absolute path because the shell reports the denial
/// itself; a write denial is reported by whatever tool hit it, with its own
/// exit code and its own idea of how to print a path — usually relative and
/// quoted:
///
/// ```text
/// fatal: cannot create '.git/index.lock': Operation not permitted
/// ```
///
/// So: take the last whitespace-separated token before the errno text, unquote
/// it, and resolve it against `cwd`. Requiring a `/` in the token is the
/// conservative half — a bare `foo.txt` is missed rather than guessed at, for
/// the same reason [`mur_common::agent_facts::AgentFacts::can_exec`] chooses to
/// under-report: a wrong path here would produce a confident, wrong hint.
pub(super) fn write_denied_path(stderr: &str, cwd: &Path) -> Option<PathBuf> {
    stderr.lines().rev().find_map(|line| {
        let head = line.trim_end().strip_suffix(": Operation not permitted")?;
        let tok = head
            .split_whitespace()
            .next_back()?
            .trim_matches(|c| c == '\'' || c == '"' || c == '`');
        if !tok.contains('/') {
            return None;
        }
        let p = Path::new(tok);
        Some(if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        })
    })
}

/// What can be said about a write denial from facts already on disk.
#[derive(Debug, PartialEq)]
pub(super) enum WriteDenial {
    /// No write grant covers the path.
    NotGranted,
    /// A grant covers it, but that grant's own path is gone. The sandbox
    /// discards grants for missing paths when it seals (Issue 16), so this one
    /// never reached the kernel however it looks in `profile.yaml`.
    GrantDiscarded { grant: PathBuf },
    /// A grant covers it and the profile has changed since this agent sealed.
    /// A seatbelt profile cannot be widened after `sandbox_init`, so a grant
    /// added since start is inert until restart.
    NeedsRestart,
}

/// Classify against a write list, given the agent's directory.
///
/// Returns `None` when nothing here explains the denial — a read-only mount and
/// a stale NFS handle also return EPERM, and a hint that claims a cause it
/// cannot support is worse than silence.
pub(super) fn classify_write_denial(
    writes: &[PathBuf],
    path: &Path,
    agent_dir: &Path,
) -> Option<WriteDenial> {
    let Some(grant) = writes.iter().find(|w| path.starts_with(w)) else {
        return Some(WriteDenial::NotGranted);
    };
    if !grant.exists() {
        return Some(WriteDenial::GrantDiscarded {
            grant: grant.clone(),
        });
    }
    // `running.lock` is written once, at startup, and never updated (one
    // production writer — supervisor's `write_lock`), so its mtime is when the
    // sandbox sealed. A newer profile means the grants moved since.
    let sealed = std::fs::metadata(agent_dir.join("running.lock"))
        .and_then(|m| m.modified())
        .ok()?;
    let edited = std::fs::metadata(agent_dir.join("profile.yaml"))
        .and_then(|m| m.modified())
        .ok()?;
    (edited > sealed).then_some(WriteDenial::NeedsRestart)
}

/// State a fact and name the command that acts on it — never assert the cause.
///
/// "This path is not under a write grant" is checkable. "That is why the
/// command failed" is not: the same errno arrives from a read-only mount.
pub(super) fn write_denied_hint(path: &Path, agent: &str, d: &WriteDenial) -> String {
    let p = path.display();
    match d {
        WriteDenial::NotGranted => {
            // `mur agent perm allow-write` refuses a path that does not exist,
            // and the denied path usually does not (that is why it was being
            // created), so name the nearest ancestor that does.
            let target = path
                .ancestors()
                .find(|a| a.exists())
                .unwrap_or(path)
                .display()
                .to_string();
            format!(
                "\n\n[sandbox] {p} is not under any write grant for agent '{agent}'.\n\
                 To grant it:\n    mur agent perm allow-write {target}\n    \
                 mur agent restart {agent}"
            )
        }
        WriteDenial::GrantDiscarded { grant } => format!(
            "\n\n[sandbox] agent '{agent}' has a write grant for {} that covers {p}, but that \
             path does not exist, so the sandbox discarded the grant when it sealed. Create it \
             (or re-grant an existing path), then restart '{agent}'.",
            grant.display()
        ),
        WriteDenial::NeedsRestart => format!(
            "\n\n[sandbox] agent '{agent}' has a write grant covering {p}, and its profile has \
             changed since the agent started. A sandbox cannot be widened after startup, so a \
             grant added since then is inert until:\n    mur agent restart {agent}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// The message from the session that prompted this: relative and quoted.
    #[test]
    fn write_denial_resolves_a_relative_quoted_path_against_cwd() {
        let out = write_denied_path(
            "fatal: cannot create '.git/index.lock': Operation not permitted\n",
            Path::new("/repo"),
        );
        assert_eq!(out.as_deref(), Some(Path::new("/repo/.git/index.lock")));
    }

    #[test]
    fn write_denial_takes_an_absolute_path_as_written() {
        let out = write_denied_path(
            "touch: /etc/hosts: Operation not permitted\n",
            Path::new("/repo"),
        );
        assert_eq!(out.as_deref(), Some(Path::new("/etc/hosts")));
    }

    /// The control that keeps this from decorating every failed command.
    #[test]
    fn write_denial_ignores_ordinary_failures() {
        for e in [
            "error: could not compile `mur-core`",
            "fatal: not a git repository",
            "test result: FAILED. 1 failed",
            "bash: frobnicate: command not found",
            "",
        ] {
            assert_eq!(
                write_denied_path(e, Path::new("/repo")),
                None,
                "fired on: {e}"
            );
        }
    }

    /// Deliberately under-reports rather than guessing — see the doc comment.
    #[test]
    fn write_denial_skips_a_token_that_is_not_path_shaped() {
        assert_eq!(
            write_denied_path("something: Operation not permitted\n", Path::new("/repo")),
            None
        );
    }

    fn stamp(p: &Path, ago: u64) {
        std::fs::write(p, "x").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(ago))
            .unwrap();
    }

    #[test]
    fn classify_reports_a_path_no_grant_covers() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(
            classify_write_denial(&[PathBuf::from("/other")], Path::new("/repo/x"), td.path()),
            Some(WriteDenial::NotGranted)
        );
    }

    /// The `cc-proxy` case: the profile lists it, the sandbox dropped it.
    #[test]
    fn classify_reports_a_grant_whose_own_path_is_gone() {
        let td = tempfile::tempdir().unwrap();
        let gone = td.path().join("vanished");
        assert_eq!(
            classify_write_denial(
                std::slice::from_ref(&gone),
                &gone.join("deep/file"),
                td.path()
            ),
            Some(WriteDenial::GrantDiscarded { grant: gone })
        );
    }

    #[test]
    fn classify_reports_a_profile_edited_after_the_seal() {
        let td = tempfile::tempdir().unwrap();
        let grant = td.path().join("tree");
        std::fs::create_dir(&grant).unwrap();
        stamp(&td.path().join("running.lock"), 600); // sealed ten minutes ago
        stamp(&td.path().join("profile.yaml"), 1); // granted a second ago
        assert_eq!(
            classify_write_denial(std::slice::from_ref(&grant), &grant.join("f"), td.path()),
            Some(WriteDenial::NeedsRestart)
        );
    }

    /// The control that matters most: a live grant, an unchanged profile, and
    /// an EPERM this cannot account for — a read-only mount returns the same
    /// errno. Saying nothing beats naming a cause we cannot support.
    #[test]
    fn classify_says_nothing_when_it_cannot_explain_the_denial() {
        let td = tempfile::tempdir().unwrap();
        let grant = td.path().join("tree");
        std::fs::create_dir(&grant).unwrap();
        stamp(&td.path().join("profile.yaml"), 600);
        stamp(&td.path().join("running.lock"), 1); // sealed after the last edit
        assert_eq!(
            classify_write_denial(std::slice::from_ref(&grant), &grant.join("f"), td.path()),
            None
        );
    }

    /// `perm allow-write` refuses a path that does not exist, so the hint must
    /// not suggest one — the denied path itself almost never exists.
    #[test]
    fn hint_names_an_ancestor_that_actually_exists() {
        let td = tempfile::tempdir().unwrap();
        let missing = td.path().join("a/b/c.lock");
        let h = write_denied_hint(&missing, "mur", &WriteDenial::NotGranted);
        assert!(
            h.contains(&format!("allow-write {}", td.path().display())),
            "{h}"
        );
        assert!(
            !h.contains("c.lock\n"),
            "must not suggest granting the missing file: {h}"
        );
    }
}
