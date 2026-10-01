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
//! When the install is absent the launch falls back to `npx`, so a machine
//! that has not re-run setup keeps today's behaviour outside a seal.

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

/// Program and argv for the server. Pure, so the choice is testable without
/// Node or an install on disk.
pub fn launch_argv(entry: Option<&Path>, extra_args: &[String]) -> (String, Vec<String>) {
    let mut args = match entry {
        Some(script) => {
            // `node <script>`, not the script itself: its `#!/usr/bin/env
            // node` would consult PATH, which a seal does not control.
            return (
                "node".to_owned(),
                std::iter::once(script.display().to_string())
                    .chain(extra_args.iter().cloned())
                    .collect(),
            );
        }
        None => vec!["-y".to_owned(), crate::PLAYWRIGHT_MCP_PKG.to_owned()],
    };
    args.extend(extra_args.iter().cloned());
    ("npx".to_owned(), args)
}

/// The installed entry for this process's MUR home, if any.
pub fn system_entry() -> Option<PathBuf> {
    crate::paths::system_mur_home().and_then(|h| installed_entry(&install_dir(&h)))
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
        let (prog, args) = launch_argv(Some(Path::new("/i/cli.js")), &s(&["--headless"]));
        assert_eq!(prog, "node");
        assert_eq!(args, s(&["/i/cli.js", "--headless"]));
        assert!(!args.iter().any(|a| a == "-y" || a.contains('@')));
    }

    #[test]
    fn uninstalled_launch_falls_back_to_pinned_npx() {
        let (prog, args) = launch_argv(None, &s(&["--headless"]));
        assert_eq!(prog, "npx");
        assert_eq!(args, s(&["-y", crate::PLAYWRIGHT_MCP_PKG, "--headless"]));
    }
}
