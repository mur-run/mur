//! The spawn grants `mur browser` needs, and the consent that applies them.
//!
//! Step 6 of setup (`/browser --add` only attaches the skill and points
//! here). Four grants, and none of them is optional for a working replay:
//!
//! * `node` — what `record`/`replay` spawn: the MCP server is launched as
//!   `node <install>/…/cli.js` (`mur_browser::server`), never through `npx`.
//! * read on that install dir — Landlock is a read allowlist, so without it
//!   `node` starts and then cannot open the script.
//! * `chrome-headless-shell` — the browser that server launches.
//! * `<mur_home>/artifacts/<agent>/shim/probe` — a *directory* grant, not a
//!   write grant. The probe binary is written at run time and then exec'd;
//!   `~/.mur` is outside the exec lane, so a write entitlement alone leaves
//!   the exec failing with `Operation not permitted`.
//! * Playwright's browsers cache (`ms-playwright`) — both an exec lane and a
//!   write grant, and both on the cache *root*, not on one revision's build
//!   dir: allowlisting `chrome-headless-shell` by name is not enough once the
//!   seal is on (the exec is by absolute path under the cache), and the
//!   server writes its `browser@<hash>` registry locks there on every launch.
//!   Scoping to a build dir would break on the next `install-browser`, so a
//!   working replay after an upgrade needs the root. Skipped when the cache
//!   cannot be located (`PLAYWRIGHT_BROWSERS_PATH=0`, no `HOME`), so nothing
//!   is guessed — the live test then reports the denial.
//!
//! [`plan`] is pure over the profile it is handed, so tests need no agent on
//! disk and nothing is granted without a literal `yes` (`cmd::consent`).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;

/// Binaries `mur browser` must be allowed to spawn.
pub const REQUIRED_BINARIES: [&str; 2] = ["node", "chrome-headless-shell"];

/// `<mur_home>/artifacts/<agent>/shim/probe` — the exec lane for the probe.
pub fn probe_dir(mur_home: &Path, agent: &str) -> PathBuf {
    mur_home
        .join("artifacts")
        .join(agent)
        .join("shim")
        .join("probe")
}

/// What the agent's profile is still missing. Empty `Plan` = nothing to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Binaries absent from `entitlements.processes.spawn.allowed`.
    pub binaries: Vec<String>,
    /// Exec lanes absent from `spawn.allowed_dirs`: the probe dir, and the
    /// browsers cache when it is locatable.
    pub dirs: Vec<String>,
    /// MCP server install dir, when absent from `filesystem.read`.
    pub read: Option<String>,
    /// Browsers cache, when absent from `filesystem.write`.
    pub write: Option<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
            && self.dirs.is_empty()
            && self.read.is_none()
            && self.write.is_none()
    }
}

/// Diff the required grants against what the profile already allows.
///
/// Pure: `allowed` / `allowed_dirs` / `reads` / `writes` come from the caller,
/// as does `browsers` (the Playwright cache, `None` when unlocatable), so this
/// never reads or writes a profile. Matching is exact, like `perm deny-spawn`'s —
/// a near match must show up as missing rather than be silently accepted.
pub fn plan(
    mur_home: &Path,
    agent: &str,
    browsers: Option<&Path>,
    allowed: &[String],
    allowed_dirs: &[String],
    reads: &[String],
    writes: &[String],
) -> Plan {
    let binaries = REQUIRED_BINARIES
        .iter()
        .filter(|b| !allowed.iter().any(|a| a == *b))
        .map(|b| (*b).to_string())
        .collect();
    let cache = browsers.map(|d| d.to_string_lossy().into_owned());
    let dirs = [
        Some(probe_dir(mur_home, agent).to_string_lossy().into_owned()),
        cache.clone(),
    ]
    .into_iter()
    .flatten()
    .filter(|want| !allowed_dirs.contains(want))
    .collect();
    let server = mur_browser::server::install_dir(mur_home)
        .to_string_lossy()
        .into_owned();
    let read = (!reads.contains(&server)).then_some(server);
    let write = cache.filter(|c| !writes.contains(c));
    Plan {
        binaries,
        dirs,
        read,
        write,
    }
}

/// One grant to apply. The two arms map to the two `perm` subcommands,
/// which differ in kind (binary allowlist vs. exec lane), not just in value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    Binary(String),
    Dir(String),
    Read(String),
    Write(String),
}

impl Plan {
    /// The missing grants, binaries first, in the order they are applied.
    pub fn grants(&self) -> Vec<Grant> {
        let mut out: Vec<Grant> = self.binaries.iter().cloned().map(Grant::Binary).collect();
        out.extend(self.dirs.iter().cloned().map(Grant::Dir));
        if let Some(r) = &self.read {
            out.push(Grant::Read(r.clone()));
        }
        if let Some(w) = &self.write {
            out.push(Grant::Write(w.clone()));
        }
        out
    }
}

/// The equivalent command — printed before asking, and the fallback a
/// non-interactive caller runs by hand.
pub fn command_for(agent: &str, grant: &Grant) -> String {
    match grant {
        Grant::Binary(b) => format!("mur agent perm allow-spawn {agent} {b}"),
        Grant::Dir(d) => format!("mur agent perm allow-spawn-dir {agent} {d}"),
        Grant::Read(r) => format!("mur agent perm allow-read {agent} {r}"),
        Grant::Write(w) => format!("mur agent perm allow-write {agent} {w}"),
    }
}

/// Every command for `plan`, one per line.
pub fn commands(agent: &str, plan: &Plan) -> Vec<String> {
    plan.grants()
        .iter()
        .map(|g| command_for(agent, g))
        .collect()
}

/// Applies one grant. Injected so tests never touch a real profile.
pub type Granter<'a> = &'a mut dyn FnMut(&Grant) -> Result<()>;

/// Show the grants, ask once, apply on a literal `yes`.
///
/// A refusal is **not** an error: the skill is attached and the browser is
/// installed either way, so setup prints the commands and carries on to the
/// live test rather than unwinding work the user already consented to.
///
/// `pre_approved` skips the question, for `--yes`. It is not a way to grant
/// silently: the grants are still printed first, and the only caller that
/// can set it is a human passing the flag, or an agent whose `mur` spawn the
/// HITL gate already put in front of that human.
pub fn confirm_and_apply(
    agent: &str,
    plan: &Plan,
    pre_approved: bool,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    grant: Granter<'_>,
) -> Result<bool> {
    if plan.is_empty() {
        writeln!(output, "\nPermissions: already granted for '{agent}'.")?;
        return Ok(true);
    }
    let cmds = commands(agent, plan);
    writeln!(
        output,
        "\n'{agent}' still needs these grants to run a browser:"
    )?;
    for c in &cmds {
        writeln!(output, "    {c}")?;
    }
    if !plan.dirs.is_empty() {
        writeln!(
            output,
            "    note      the directory grant is an exec lane, not a write grant"
        )?;
    }
    if !pre_approved && !crate::cmd::consent::literal_yes(input, output)? {
        writeln!(output, "  skipped — run the commands above when ready.")?;
        return Ok(false);
    }
    for g in plan.grants() {
        grant(&g)?;
    }
    writeln!(output, "  ✓ granted")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        PathBuf::from("/tmp/murhome")
    }

    /// The expected probe dir, spelled the way the platform spells it:
    /// `probe_dir` joins components, so the separator is `\` on Windows.
    fn probe(agent: &str) -> String {
        probe_dir(&home(), agent).to_string_lossy().into_owned()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    /// The cache a mac/Linux install resolves; tests pass it explicitly so
    /// they do not depend on the host's `HOME` or `PLAYWRIGHT_BROWSERS_PATH`.
    fn cache() -> PathBuf {
        home().join("ms-playwright")
    }

    fn cache_s() -> String {
        cache().to_string_lossy().into_owned()
    }

    fn server() -> String {
        mur_browser::server::install_dir(&home())
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn empty_profile_needs_every_grant() {
        let p = plan(&home(), "mur", Some(&cache()), &[], &[], &[], &[]);
        assert_eq!(p.binaries, s(&["node", "chrome-headless-shell"]));
        assert_eq!(p.dirs, vec![probe("mur"), cache_s()]);
        assert_eq!(p.read, Some(server()));
        assert_eq!(p.write, Some(cache_s()));
        assert!(!p.is_empty());
    }

    #[test]
    fn fully_granted_profile_is_a_no_op() {
        let dirs = vec![probe("mur"), cache_s()];
        let p = plan(
            &home(),
            "mur",
            Some(&cache()),
            &s(&["node", "chrome-headless-shell"]),
            &dirs,
            &[server()],
            &[cache_s()],
        );
        assert!(p.is_empty(), "{p:?}");
    }

    /// The probe dir is per-agent: another agent's grant must not count.
    #[test]
    fn dir_grant_is_not_shared_between_agents() {
        let dirs = vec![probe("other"), cache_s()];
        let p = plan(
            &home(),
            "mur",
            Some(&cache()),
            &[],
            &dirs,
            &[server()],
            &[cache_s()],
        );
        assert_eq!(p.dirs, vec![probe("mur")]);
    }

    #[test]
    fn partial_profile_only_lists_what_is_missing() {
        let p = plan(
            &home(),
            "mur",
            Some(&cache()),
            &s(&["node"]),
            &[cache_s()],
            &[server()],
            &[cache_s()],
        );
        assert_eq!(p.binaries, s(&["chrome-headless-shell"]));
        let cmds = commands("mur", &p);
        assert_eq!(cmds.len(), 2, "{cmds:?}");
        assert!(cmds[0].ends_with("allow-spawn mur chrome-headless-shell"));
        assert!(cmds[1].contains(&format!("allow-spawn-dir mur {}", probe("mur"))));
    }

    /// The seal execs the shell by absolute path under the cache and the
    /// server writes its registry locks there, so both lanes are required
    /// even when every binary is already allowlisted by name.
    #[test]
    fn browsers_cache_needs_both_exec_and_write() {
        let p = plan(
            &home(),
            "mur",
            Some(&cache()),
            &s(&["node", "chrome-headless-shell"]),
            &[probe("mur")],
            &[server()],
            &[],
        );
        let cmds = commands("mur", &p);
        assert!(
            cmds.contains(&format!("mur agent perm allow-spawn-dir mur {}", cache_s())),
            "{cmds:?}"
        );
        assert!(
            cmds.contains(&format!("mur agent perm allow-write mur {}", cache_s())),
            "{cmds:?}"
        );
    }

    /// `PLAYWRIGHT_BROWSERS_PATH=0` (or no `HOME`) leaves the cache
    /// unlocatable: nothing about it is planned rather than a path guessed.
    #[test]
    fn unlocatable_cache_is_skipped_not_guessed() {
        let p = plan(
            &home(),
            "mur",
            None,
            &s(&["node", "chrome-headless-shell"]),
            &[probe("mur")],
            &[server()],
            &[],
        );
        assert!(p.is_empty(), "{p:?}");
    }

    /// The launch chain is `node <install>/…`; the npx-era bare name
    /// `playwright-mcp` never resolves to an executable and must not count.
    #[test]
    fn grants_match_the_vendored_launch_chain() {
        let p = plan(
            &home(),
            "mur",
            Some(&cache()),
            &s(&["playwright-mcp"]),
            &[],
            &[],
            &[],
        );
        assert!(p.binaries.contains(&"node".to_string()), "{p:?}");
        let cmds = commands("mur", &p);
        assert!(
            cmds.iter()
                .any(|c| c == &format!("mur agent perm allow-read mur {}", server())),
            "{cmds:?}"
        );
    }

    fn run(answer: &str, p: &Plan) -> (bool, String, Vec<String>) {
        let mut out = Vec::new();
        let mut applied = Vec::new();
        let mut grant = |g: &Grant| {
            applied.push(command_for("mur", g));
            Ok(())
        };
        let ok = confirm_and_apply(
            "mur",
            p,
            false,
            &mut answer.as_bytes(),
            &mut out,
            &mut grant,
        )
        .unwrap();
        (ok, String::from_utf8(out).unwrap(), applied)
    }

    #[test]
    fn nothing_is_granted_without_a_literal_yes() {
        let p = plan(
            &home(),
            "mur",
            Some(Path::new("/tmp/ms-playwright")),
            &[],
            &[],
            &[],
            &[],
        );
        for a in ["y\n", "YES\n", "\n", "", "no\n"] {
            let (ok, out, applied) = run(a, &p);
            assert!(!ok, "{a:?} must not consent");
            assert!(applied.is_empty(), "{a:?} granted {applied:?}");
            assert!(out.contains("run the commands above"), "{out}");
        }
    }

    #[test]
    fn a_literal_yes_applies_every_missing_grant() {
        let p = plan(
            &home(),
            "mur",
            Some(Path::new("/tmp/ms-playwright")),
            &[],
            &[],
            &[],
            &[],
        );
        let (ok, out, applied) = run("yes\n", &p);
        assert!(ok);
        assert_eq!(applied, commands("mur", &p));
        assert!(out.contains("exec lane"), "{out}");
        assert!(out.contains("✓ granted"), "{out}");
    }

    #[test]
    fn an_already_granted_profile_asks_nothing() {
        let (ok, out, applied) = run("no\n", &Plan::default());
        assert!(ok, "a no-op must not read as refusal");
        assert!(applied.is_empty());
        assert!(out.contains("already granted"), "{out}");
    }
}
