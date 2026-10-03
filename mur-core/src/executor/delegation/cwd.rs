//! The target directory of a delegation and the routing note that tells a
//! member about it. Shared by `mur fleet run` and `parallel_jobs` so both
//! resolve the CALLER's directory, never this process's (#1607).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Where the work is: the directory members are routed to. `None` means the
/// caller gave none and this process's cwd stands in — right for a human at a
/// shell, wrong for a spawned child, which is why the runtime always passes it.
#[derive(Debug, Clone, Default)]
pub struct RunCwd {
    pub path: Option<PathBuf>,
    /// The caller did not name the directory; it was taken from the calling
    /// agent's session cwd. Surfaces in the routing note so a member (and the
    /// log) can tell a stated target from a guessed one.
    pub inferred: bool,
}

impl RunCwd {
    /// Build from a tool call's arguments: an optional absolute `cwd` and the
    /// runtime's `cwd_inferred` flag. At a tool boundary no `cwd` at all means
    /// the serving process's cwd will stand in, which is itself a guess — so
    /// that counts as inferred too. (A human at a shell is different: their
    /// process cwd is where they are, which is why the CLI builds the struct
    /// directly.)
    pub fn from_tool_args(cwd: Option<&str>, cwd_inferred: bool) -> Self {
        Self {
            path: cwd.map(PathBuf::from),
            inferred: cwd.is_none() || cwd_inferred,
        }
    }

    /// The directory to route to. Rejects relative and non-existent paths
    /// rather than letting them resolve against whatever cwd this process has.
    pub fn resolve(&self) -> Result<PathBuf> {
        match &self.path {
            Some(p) => {
                if !p.is_absolute() {
                    bail!("cwd must be an absolute path, got `{}`", p.display());
                }
                if !p.is_dir() {
                    bail!("cwd `{}` is not a directory", p.display());
                }
                Ok(p.clone())
            }
            None => std::env::current_dir().context("current_dir"),
        }
    }

    /// Resolve and render the routing note in one go.
    pub fn routing_note(&self) -> Result<String> {
        Ok(routing_note(&self.resolve()?, self.inferred))
    }
}

/// Main repo root (where `.worktrees/` lives), discovered from `from` — the
/// caller's working directory, never this process's. When the agent runtime
/// spawns `mur fleet run`, the process cwd is wherever the runtime happens to
/// sit (its home, `/`), not the project the user is in; resolving git there
/// routed members to the wrong repo (#1607). The CLI passes `--cwd`, or its
/// own cwd when invoked by hand.
pub fn discover_repo_root(from: &Path) -> Result<PathBuf> {
    let out = mur_common::repo_walk::bound_git(&mut std::process::Command::new("git"))
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(from)
        .output()
        .context("git rev-parse --show-toplevel")?;
    if !out.status.success() {
        bail!("parallel execution must run inside a git repository");
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// The directory a member is actually told to work in: the git root when
/// `work_dir` is inside a checkout, `work_dir` itself otherwise. The routing
/// note and the write-grant check both use this, so the directory the member
/// is sent to and the one it is checked against cannot differ.
pub fn routing_target(work_dir: &Path) -> PathBuf {
    discover_repo_root(work_dir).unwrap_or_else(|_| work_dir.to_path_buf())
}

/// The line appended to a delegated prompt telling the member where the work
/// is. Repo root when `work_dir` is inside a git checkout, the directory
/// itself otherwise — a member with no idea which tree to touch is how a build
/// dir ends up in the wrong project. An inferred cwd says so, so the member
/// and the reader of the log both know it was a guess, not an instruction.
pub fn routing_note(work_dir: &Path, inferred: bool) -> String {
    let target = routing_target(work_dir);
    let target = target.display();
    let provenance = if inferred {
        " (assumed from the calling agent's session directory — no explicit target was given)"
    } else {
        ""
    };
    format!(
        "\n\nIMPORTANT: the directory you are working in is `{target}`{provenance}. cd there (or pass cwd=`{target}` on every bash/tool call) before doing anything else."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1607: the routing note names the directory the CALLER gave, resolved to
    /// its git root, and never this process's cwd. Outside a checkout the
    /// directory itself is the target.
    #[test]
    fn routing_note_targets_the_given_dir_not_process_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("proj");
        let nested = repo.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("skipping: git unavailable");
            return;
        }
        // The note quotes git's own spelling of the root. Compare directory
        // identity via canonicalize, never the strings: macOS symlinks the
        // tmpdir, and on Windows git prints `C:/...` while canonicalize
        // returns the verbatim `\\?\C:\...` form.
        let repo_canon = repo.canonicalize().unwrap();
        let root = discover_repo_root(&nested).unwrap();

        let note = routing_note(&nested, false);
        assert!(note.contains(&format!("`{}`", root.display())), "{note}");
        assert!(
            !note.contains("assumed"),
            "explicit cwd is not a guess: {note}"
        );
        // discover_repo_root is anchored on the argument, not on where the test
        // runner happens to be.
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_ne!(cwd, repo_canon);
        assert_eq!(root.canonicalize().unwrap(), repo_canon);

        let plain = tmp.path().join("no-git");
        std::fs::create_dir_all(&plain).unwrap();
        let note = routing_note(&plain, true);
        assert!(
            note.contains(&format!("`{}`", plain.display())),
            "falls back to the dir itself: {note}"
        );
        assert!(
            note.contains("assumed from the calling agent's session directory"),
            "inferred cwd is marked: {note}"
        );
    }

    #[test]
    fn run_cwd_rejects_relative_and_missing_paths() {
        let rel = RunCwd {
            path: Some(PathBuf::from("relative/dir")),
            inferred: false,
        };
        assert!(rel.resolve().unwrap_err().to_string().contains("absolute"));
        let tmp = tempfile::tempdir().unwrap();
        let missing = RunCwd {
            path: Some(tmp.path().join("nope")),
            inferred: false,
        };
        assert!(
            missing
                .resolve()
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
        assert_eq!(
            RunCwd::default().resolve().unwrap(),
            std::env::current_dir().unwrap()
        );
    }

    /// Absent `cwd` is a guess by construction: the process cwd stands in.
    /// An explicit one is only a guess when the caller says so.
    #[test]
    fn from_tool_args_marks_absent_cwd_as_inferred() {
        assert!(RunCwd::from_tool_args(None, false).inferred);
        assert!(RunCwd::from_tool_args(Some("/x"), true).inferred);
        let explicit = RunCwd::from_tool_args(Some("/x"), false);
        assert!(!explicit.inferred);
        assert_eq!(explicit.path.as_deref(), Some(Path::new("/x")));
    }
}
