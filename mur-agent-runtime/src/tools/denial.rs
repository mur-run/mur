//! Classifying a kernel denial that reached us through a child's stderr.
//!
//! The sandbox is sealed in-kernel at process start, so a denial is not an
//! error MUR raises — it is an errno some tool we spawned printed and gave up
//! on. Without a translation the model sees `Operation not permitted` and hands
//! the command back to the user, which is the one outcome this exists to avoid.
//!
//! Both halves live here: the write denial (a path the sandbox will not let a
//! child create) and the exec denial (a binary it will not let one run).

use std::path::{Path, PathBuf};

use mur_common::agent_facts::ExecRoutes;

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

/// The binary path bash refused to exec, when the SANDBOX denied it.
///
/// bash reports `…: <path>: Operation not permitted` and exits **126**
/// ("found but not executable") — which under a sealed profile means the
/// binary is outside `entitlements.processes.spawn.allowed`. A write EPERM
/// exits 1 and a missing binary exits 127, so neither is mistaken for this.
///
/// Verified against real Seatbelt, not inferred:
/// `sandbox-exec -p '(version 1)(allow default)(deny process-exec* (subpath "/opt"))' \
///   /bin/bash -c '/opt/homebrew/bin/git --version'`
/// → `/bin/bash: /opt/homebrew/bin/git: Operation not permitted`, exit 126.
pub(super) fn spawn_denied_path(exit_code: Option<i32>, stderr: &str) -> Option<String> {
    if exit_code != Some(126) {
        return None;
    }
    stderr.lines().rev().find_map(|line| {
        let path = line
            .trim_end()
            .strip_suffix(": Operation not permitted")?
            .rsplit(": ")
            .next()?;
        path.starts_with('/').then(|| path.to_string())
    })
}

/// Turn an opaque kernel EPERM into a route that can actually resolve it. The
/// sandbox is compiled at process start and enforced in-kernel, so no runtime
/// prompt is possible (HITL is per-TOOL, not per-binary) — without this the
/// model just sees "Operation not permitted" and hands the command back to the
/// user, which is the one outcome delegation exists to avoid.
///
/// `routes` comes from [`mur_common::agent_facts::who_can_exec`], so the fleets
/// named here are the ones that provably hold this binary, best (least
/// privileged, right working directory) first. Only `ready` routes are
/// mentioned: naming fleets the agent may NOT use would hand a prompt-injected
/// agent a map of the more powerful things on this machine. The user learns
/// about those from `mur agent who`, where it is a grant path rather than a
/// target list.
pub(super) fn spawn_denied_hint(bin: &str, agent: &str, routes: &ExecRoutes) -> String {
    let name = std::path::Path::new(bin)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| bin.to_string());
    let route = if routes.ready.is_empty() {
        format!("No authorized fleet can run `{name}`, so this one needs the user.")
    } else {
        let named: Vec<String> = routes
            .ready
            .iter()
            .map(|f| {
                let via: Vec<&str> = f
                    .members_with(bin)
                    .iter()
                    .map(|m| m.name.as_str())
                    .collect();
                format!("\"{}\" (via {})", f.name, via.join(", "))
            })
            .collect();
        format!(
            "DELEGATE it instead of asking the user: \
             fleet_run(fleet={}, goal=<this exact command, with absolute paths>). \
             Best first: {}.",
            format_args!("\"{}\"", routes.ready[0].name),
            named.join(", ")
        )
    };
    let drift: Vec<String> = routes
        .ready
        .iter()
        .flat_map(|f| f.members_with(bin))
        .filter(|m| m.drift)
        .map(|m| m.name.clone())
        .collect();
    let drift_note = if drift.is_empty() {
        String::new()
    } else {
        format!(
            "\nNote: {} started before its profile was last edited, so it may not have \
             the grant yet — report that if the delegation fails.",
            drift.join(", ")
        )
    };
    format!(
        "\n\n[sandbox] `{name}` is not in agent '{agent}''s spawn allowlist, so the kernel \
         refused to exec it. This is decided when the agent starts — there is no approval \
         prompt for it. {route}{drift_note}\nTo grant it here instead, the user runs: \
         `mur agent perm allow-spawn {agent} {name}` and restarts the agent."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const DENIED: &str = "bash: line 1: /Users/d/.cargo/bin/cargo: Operation not permitted";

    #[test]
    fn spawn_denied_path_matches_only_the_kernel_exec_denial() {
        assert_eq!(
            spawn_denied_path(Some(126), DENIED).as_deref(),
            Some("/Users/d/.cargo/bin/cargo")
        );
        // Same text, but a write EPERM (exit 1) or a missing binary (127) —
        // neither is a spawn-allowlist denial, so neither gets the hint.
        assert_eq!(spawn_denied_path(Some(1), DENIED), None);
        assert_eq!(spawn_denied_path(Some(127), DENIED), None);
        // 126 without the signature (e.g. a real non-executable file).
        assert_eq!(
            spawn_denied_path(Some(126), "bash: ./x: Permission denied"),
            None
        );
        // A relative/garbled path is not a usable route — don't guess.
        assert_eq!(
            spawn_denied_path(Some(126), "bash: cargo: Operation not permitted"),
            None
        );
    }

    #[test]
    fn spawn_denied_hint_names_the_route_and_the_grant() {
        use mur_common::agent::NetworkOutboundMode;
        use mur_common::agent_facts::{AgentFacts, ExecFacts, FleetFacts};

        let member = AgentFacts {
            name: "rustsmith".into(),
            role: String::new(),
            exec: ExecFacts::Allowlist(vec!["cargo".into()]),
            writes: vec![PathBuf::from("/repo")],
            net: NetworkOutboundMode::Restricted,
            skills: vec![],
            model_ref: String::new(),
            effort: None,
            running: true,
            drift: false,
        };
        let routes = ExecRoutes {
            ready: vec![FleetFacts {
                name: "builder".into(),
                members: vec![member.clone()],
                budget_usd: 1.0,
                authorized: true,
            }],
            // An unauthorized-but-capable fleet must NEVER be named to the
            // model — that list is an attack map, not a route.
            blocked: vec![FleetFacts {
                name: "secret-powerful".into(),
                members: vec![member],
                budget_usd: 0.0,
                authorized: false,
            }],
        };
        let h = spawn_denied_hint("/Users/d/.cargo/bin/cargo", "mur", &routes);
        assert!(h.contains("fleet_run(fleet=\"builder\""), "{h}");
        assert!(h.contains("via rustsmith"), "{h}");
        assert!(h.contains("mur agent perm allow-spawn mur cargo"), "{h}");
        assert!(!h.contains("secret-powerful"), "blocked fleet leaked: {h}");

        // No usable route → never invent one.
        let h = spawn_denied_hint("/usr/bin/git", "solo", &ExecRoutes::default());
        assert!(!h.contains("fleet_run("), "{h}");
        assert!(h.contains("allow-spawn solo git"), "{h}");
    }
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
