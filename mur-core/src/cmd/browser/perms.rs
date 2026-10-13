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
//! * Playwright's browsers cache (`ms-playwright`) — an exec lane on each
//!   build dir setup installs (headless shell, full Chromium), and a write
//!   grant on the server registry only (`ms-playwright/b`, where the server
//!   writes its `browser@<hash>` descriptors on every launch; `.links` and
//!   `__dirlock` are install-time and the install runs outside the agent).
//!   Never write on the cache root: the cache is shared by every agent and by
//!   the user's own Playwright, so a dir both writable and exec'able there
//!   would let one agent plant a binary another later runs (#1730). An older
//!   setup did grant the root writable; that grant is offered back for
//!   removal. Build-dir lanes are per revision, so rerun setup after an
//!   `install-browser` upgrade. Skipped when the cache cannot be located
//!   (`PLAYWRIGHT_BROWSERS_PATH=0`, no `HOME`), so nothing is guessed — the
//!   live test then reports the denial.
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

/// Where Playwright's browsers live, as located by the caller. All `None` /
/// empty when unlocatable, so [`plan`] never guesses a path.
#[derive(Debug, Default, Clone)]
pub struct Browsers {
    /// The cache root (`ms-playwright`). Only used to spot the legacy
    /// root-wide write grant; never granted itself.
    pub cache: Option<PathBuf>,
    /// Build dirs the agent launches from (exec lanes), newest first.
    pub builds: Vec<PathBuf>,
    /// The server registry the MCP server writes on every launch.
    pub registry: Option<PathBuf>,
}

impl Browsers {
    /// Locate everything from the cache dir and the server registry dir:
    /// the headless shell replay launches and the full build `auth` needs.
    pub fn locate(cache: Option<&Path>, registry: Option<&Path>) -> Self {
        let builds = cache
            .map(|dir| {
                let mut out: Vec<PathBuf> = Vec::new();
                for name in [
                    super::doctor::usable_build(dir),
                    super::doctor::full_build(dir),
                ]
                .into_iter()
                .flatten()
                {
                    let b = dir.join(name);
                    if !out.contains(&b) {
                        out.push(b);
                    }
                }
                out
            })
            .unwrap_or_default();
        Self {
            cache: cache.map(Path::to_path_buf),
            builds,
            registry: registry.map(Path::to_path_buf),
        }
    }
}

/// What the agent's profile is still missing. Empty `Plan` = nothing to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Binaries absent from `entitlements.processes.spawn.allowed`.
    pub binaries: Vec<String>,
    /// Exec lanes absent from `spawn.allowed_dirs`: the probe dir, and the
    /// browser build dirs when the cache is locatable.
    pub dirs: Vec<String>,
    /// MCP server install dir, when absent from `filesystem.read`.
    pub read: Option<String>,
    /// Server registry, when absent from `filesystem.write`.
    pub write: Option<String>,
    /// Legacy write grant on the shared cache root, to take back (#1730).
    pub revoke_write: Option<String>,
    /// Legacy exec lane on the shared cache root, to take back (#1730): left
    /// in place it would make the registry write grant exec'able too.
    pub revoke_dir: Option<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
            && self.dirs.is_empty()
            && self.read.is_none()
            && self.write.is_none()
            && self.revoke_write.is_none()
            && self.revoke_dir.is_none()
    }
}

/// Diff the required grants against what the profile already allows.
///
/// Pure: `allowed` / `allowed_dirs` / `reads` / `writes` come from the caller,
/// as does `browsers` (located by [`Browsers::locate`]), so this never reads
/// or writes a profile. Matching is exact, like `perm deny-spawn`'s — a near
/// match must show up as missing rather than be silently accepted.
pub fn plan(
    mur_home: &Path,
    agent: &str,
    browsers: &Browsers,
    allowed: &[String],
    allowed_dirs: &[String],
    reads: &[String],
    writes: &[String],
) -> Plan {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let binaries = REQUIRED_BINARIES
        .iter()
        .filter(|b| !allowed.iter().any(|a| a == *b))
        .map(|b| (*b).to_string())
        .collect();
    let dirs = std::iter::once(probe_dir(mur_home, agent))
        .chain(browsers.builds.iter().cloned())
        .map(|d| s(d.as_path()))
        .filter(|want| !allowed_dirs.contains(want))
        .collect();
    let server = s(&mur_browser::server::install_dir(mur_home));
    let read = (!reads.contains(&server)).then_some(server);
    let write = browsers
        .registry
        .as_deref()
        .map(s)
        .filter(|r| !writes.contains(r));
    let cache = browsers.cache.as_deref().map(s);
    let revoke_write = cache.clone().filter(|c| writes.contains(c));
    let revoke_dir = cache.filter(|c| allowed_dirs.contains(c));
    Plan {
        binaries,
        dirs,
        read,
        write,
        revoke_write,
        revoke_dir,
    }
}

/// One change to apply. The arms map to the `perm` subcommands, which differ
/// in kind (binary allowlist vs. exec lane), not just in value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    Binary(String),
    Dir(String),
    Read(String),
    Write(String),
    /// Take a write grant back (`perm remove-path … write`).
    RevokeWrite(String),
    /// Take an exec lane back (`perm deny-spawn-dir`).
    RevokeDir(String),
}

impl Plan {
    /// The changes, revocation first so a refusal mid-way never leaves the
    /// overlap in place with more added on top; then binaries, lanes, paths.
    pub fn grants(&self) -> Vec<Grant> {
        let mut out: Vec<Grant> = self
            .revoke_write
            .iter()
            .cloned()
            .map(Grant::RevokeWrite)
            .collect();
        out.extend(self.revoke_dir.iter().cloned().map(Grant::RevokeDir));
        out.extend(self.binaries.iter().cloned().map(Grant::Binary));
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
        Grant::RevokeWrite(w) => format!("mur agent perm remove-path {agent} write {w}"),
        Grant::RevokeDir(d) => format!("mur agent perm deny-spawn-dir {agent} {d}"),
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
    if plan.revoke_write.is_some() || plan.revoke_dir.is_some() {
        writeln!(
            output,
            "    note      the first lines take back old grants on the shared browsers cache:\n              \
             writable and exec'able together, it lets one agent plant a binary another runs"
        )?;
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
#[path = "perms_tests.rs"]
mod tests;
