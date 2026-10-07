//! How the downstream `@playwright/mcp` server is launched.
//!
//! `npx -y @playwright/mcp@<v>` resolves the package at every spawn. Inside
//! an agent's seal that resolution is a registry request (`registry.npmjs.org`
//! is not on a Restricted allowlist, and should not be), so the server never
//! starts. Allowing the registry would trade the failure for an unpinned,
//! network-dependent launch on every tool call.
//!
//! Instead `mur browser setup` installs the pinned version once, outside any
//! seal, into a directory MUR owns ([`install_dir`]). Every later spawn runs
//! `node <install>/node_modules/@playwright/mcp/<bin>` directly: no resolution
//! step, no network, the same bytes each time. Same shape as
//! `mur agent mcp vendor`, specialised to the one server `mur browser` owns.
//!
//! When the install is absent there is no fallback: the launch fails with an
//! error that names `mur browser setup`. Falling back to `npx` would fail
//! inside a seal anyway (registry egress, read-only npm cache), and with an
//! npm error that never mentions setup.
//!
//! Old versions are not removed: bumping [`VERSION`] installs beside the
//! previous tree, which stays until the user deletes it.
//!
//! The install runs `npm install --ignore-scripts` on purpose, so npm executes
//! no package code at install time. `@playwright/mcp@0.0.82` and its two
//! dependencies (`playwright`, `playwright-core`) declare no install scripts;
//! if a later pin starts to depend on one, that flag is the first place to look.

use std::path::{Path, PathBuf};

/// npm package name, without the version.
pub const PACKAGE: &str = "@playwright/mcp";
/// Pinned version; see [`crate::PLAYWRIGHT_MCP_PKG`] for why it is pinned.
pub const VERSION: &str = "0.0.82";

/// `<mur_home>/browser/mcp-server/<VERSION>` — versioned, so bumping the pin
/// installs beside the old tree instead of mutating it under a running agent.
pub fn install_dir(mur_home: &Path) -> PathBuf {
    crate::paths::browser_root(mur_home)
        .join("mcp-server")
        .join(VERSION)
}

/// The installed entry script, when `install_dir` holds the pinned version.
///
/// Reads the package's own `package.json` rather than assuming a file name:
/// `bin` is either a string or a name → path map, and a version mismatch
/// (a stale tree from an older pin) must count as "not installed".
pub fn installed_entry(install_dir: &Path) -> Option<PathBuf> {
    let pkg_dir = install_dir.join("node_modules").join(PACKAGE);
    let raw = std::fs::read_to_string(pkg_dir.join("package.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    if v.get("version").and_then(|x| x.as_str()) != Some(VERSION) {
        return None;
    }
    let rel = match v.get("bin")? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => {
            let short = PACKAGE.rsplit('/').next().unwrap_or(PACKAGE);
            let named = format!("playwright-{short}");
            map.get(&named)
                .or_else(|| map.get(short))
                .or_else(|| (map.len() == 1).then(|| map.values().next()).flatten())?
                .as_str()?
                .to_owned()
        }
        _ => return None,
    };
    let entry = pkg_dir.join(rel);
    entry.is_file().then_some(entry)
}

/// Directories a bare `node` is resolved against, in order, BEFORE the
/// inherited `PATH`. Deliberately the same fixed list the agent seal searches
/// to turn `allow-spawn node` into an exec grant
/// (`mur-agent-runtime`'s `exec_dirs::standard_exec_dirs`) — the two must
/// answer the same question or the grant names one binary and the spawn runs
/// another.
///
/// That drift is the bug this exists for: a version-manager shim dir early on
/// the user's own `PATH` (nvm, volta, BitL, …) owns `node`, the seal never
/// granted that copy, and `mur browser record --headless` died with
/// `Operation not permitted (os error 1)` under a profile that plainly lists
/// `node` as allowed.
///
/// Trade-off, same as the runtime's: under a seal the MCP server runs the
/// `node` from these fixed dirs, not the version a shim dir selected.
/// The interpreter's file name: Windows needs the `.exe` suffix to find it.
const NODE_BIN: &str = if cfg!(windows) { "node.exe" } else { "node" };

const NODE_SEARCH_DIRS: [&str; 3] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

/// Program and argv for the server. Pure, so the choice is testable without
/// Node or an install on disk.
///
/// `node <script>`, not the script itself: its `#!/usr/bin/env node` would
/// consult PATH for the interpreter. The interpreter is resolved through
/// [`node_program`], so the path spawned is the one the seal granted.
pub fn launch_argv(entry: &Path, extra_args: &[String]) -> (String, Vec<String>) {
    (
        node_program(&NODE_SEARCH_DIRS.map(PathBuf::from), &|p| {
            is_executable_file(p)
        }),
        std::iter::once(entry.display().to_string())
            .chain(extra_args.iter().cloned())
            .collect(),
    )
}

/// The `node` to exec: the first executable `node` in `search_dirs`, else the
/// bare name so a machine that keeps Node somewhere else still works through
/// `PATH` (unsealed runs, Windows, a custom prefix).
///
/// Pure over `search_dirs` and `is_exec` so the ordering contract is testable
/// without a real Node install.
fn node_program(search_dirs: &[PathBuf], is_exec: &dyn Fn(&Path) -> bool) -> String {
    search_dirs
        .iter()
        .map(|d| d.join(NODE_BIN))
        .find(|p| is_exec(p))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| NODE_BIN.to_owned())
}

/// Is `p` a regular file with an execute bit? (Existence alone is not enough:
/// a Homebrew keg can leave a non-executable stub behind.)
fn is_executable_file(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// The installed entry under `mur_home`, or an error that says how to fix it.
pub fn require_entry(mur_home: Option<&Path>) -> anyhow::Result<PathBuf> {
    let Some(home) = mur_home else {
        anyhow::bail!("cannot locate the MUR home, so the browser MCP server cannot be found");
    };
    let dir = install_dir(home);
    installed_entry(&dir).ok_or_else(|| missing_install_error(&dir))
}

/// Shown when the pinned server is not installed. It names the fix and where
/// to run it: setup needs npm and the registry, which an agent's seal does not
/// grant, so running it from inside an agent fails.
pub fn missing_install_error(dir: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "the browser MCP server ({PACKAGE}@{VERSION}) is not installed at {}. \
         Run `mur browser setup` in a terminal, outside any agent (it needs npm \
         and the npm registry, which an agent's sandbox does not allow), then retry.",
        dir.display()
    )
}

/// The installed entry for this process's MUR home.
pub fn system_entry() -> anyhow::Result<PathBuf> {
    require_entry(crate::paths::system_mur_home().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    fn fake_install(dir: &Path, version: &str, bin: serde_json::Value) {
        let pkg = dir.join("node_modules").join(PACKAGE);
        std::fs::create_dir_all(&pkg).unwrap();
        let manifest = serde_json::json!({ "name": PACKAGE, "version": version, "bin": bin });
        std::fs::write(pkg.join("package.json"), manifest.to_string()).unwrap();
        std::fs::write(pkg.join("cli.js"), "").unwrap();
    }

    #[test]
    fn version_and_package_agree_with_the_pinned_spec() {
        assert_eq!(crate::PLAYWRIGHT_MCP_PKG, format!("{PACKAGE}@{VERSION}"));
    }

    #[test]
    fn install_dir_is_versioned_under_the_browser_root() {
        let dir = install_dir(Path::new("/h"));
        assert_eq!(
            dir,
            Path::new("/h")
                .join("browser")
                .join("mcp-server")
                .join(VERSION)
        );
    }

    #[test]
    fn installed_entry_reads_the_bin_map() {
        let t = tempfile::tempdir().unwrap();
        fake_install(
            t.path(),
            VERSION,
            serde_json::json!({ "playwright-mcp": "cli.js" }),
        );
        let entry = installed_entry(t.path()).unwrap();
        assert!(entry.ends_with("cli.js"));
    }

    #[test]
    fn installed_entry_accepts_a_string_bin() {
        let t = tempfile::tempdir().unwrap();
        fake_install(t.path(), VERSION, serde_json::json!("cli.js"));
        assert!(installed_entry(t.path()).is_some());
    }

    #[test]
    fn a_stale_version_is_not_installed() {
        let t = tempfile::tempdir().unwrap();
        fake_install(t.path(), "0.0.1", serde_json::json!("cli.js"));
        assert!(installed_entry(t.path()).is_none());
    }

    #[test]
    fn a_missing_install_is_not_installed() {
        let t = tempfile::tempdir().unwrap();
        assert!(installed_entry(t.path()).is_none());
    }

    #[test]
    fn installed_launch_is_node_on_the_script_with_no_npx() {
        let (prog, args) = launch_argv(Path::new("/i/cli.js"), &s(&["--headless"]));
        assert!(
            prog == NODE_BIN || Path::new(&prog).file_name().unwrap() == NODE_BIN,
            "{prog}"
        );
        assert_eq!(args, s(&["/i/cli.js", "--headless"]));
        assert!(!args.iter().any(|a| a == "-y" || a.contains('@')));
    }

    /// The node-spawn bug: resolving `node` through the inherited `PATH` let a
    /// version-manager shim dir win, and the seal had granted the fixed-dir
    /// copy instead — `Operation not permitted (os error 1)` under a profile
    /// that allows `node`. The program must come from the fixed search dirs,
    /// in their order, regardless of what `PATH` says.
    #[test]
    fn node_comes_from_the_seal_search_dirs_in_order() {
        let dirs = [
            PathBuf::from("/shim/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/bin"),
        ];
        // Only the two non-shim dirs hold an executable node.
        let is_exec = |p: &Path| p.starts_with("/opt/homebrew") || p.starts_with("/usr/bin");
        let node_in = |d: &Path| d.join(NODE_BIN).display().to_string();
        assert_eq!(node_program(&dirs, &is_exec), node_in(&dirs[1]));
        // Earlier dirs win when they do have one.
        assert_eq!(node_program(&dirs, &|_: &Path| true), node_in(&dirs[0]));
    }

    /// No `node` in any fixed dir is not a failure: fall back to the bare
    /// name so an unsealed run resolves it through `PATH` as before.
    #[test]
    fn no_node_in_the_search_dirs_falls_back_to_the_bare_name() {
        let dirs = [PathBuf::from("/nowhere")];
        assert_eq!(node_program(&dirs, &|_: &Path| false), NODE_BIN);
    }

    /// A non-executable stub must not be picked: it would exec-fail at spawn.
    /// Unix-only: Windows has no exec bit, so any file there counts.
    #[cfg(unix)]
    #[test]
    fn a_non_executable_node_is_skipped() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join(NODE_BIN), "").unwrap();
        let dirs = [t.path().to_path_buf()];
        assert_eq!(node_program(&dirs, &|p| is_executable_file(p)), NODE_BIN);
    }

    #[test]
    fn a_missing_install_is_an_error_naming_setup_not_an_npx_fallback() {
        let t = tempfile::tempdir().unwrap();
        let err = require_entry(Some(t.path())).unwrap_err().to_string();
        assert!(err.contains("mur browser setup"), "{err}");
        assert!(err.contains("outside any agent"), "{err}");
        assert!(
            err.contains(&install_dir(t.path()).display().to_string()),
            "{err}"
        );
        assert!(!err.contains("npx"), "{err}");
    }

    #[test]
    fn a_stale_install_is_the_same_error() {
        let t = tempfile::tempdir().unwrap();
        fake_install(&install_dir(t.path()), "0.0.1", serde_json::json!("cli.js"));
        let err = require_entry(Some(t.path())).unwrap_err().to_string();
        assert!(err.contains("mur browser setup"), "{err}");
    }

    #[test]
    fn an_installed_tree_resolves() {
        let t = tempfile::tempdir().unwrap();
        fake_install(&install_dir(t.path()), VERSION, serde_json::json!("cli.js"));
        assert!(require_entry(Some(t.path())).unwrap().ends_with("cli.js"));
    }
}
