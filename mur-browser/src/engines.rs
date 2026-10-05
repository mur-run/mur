//! Which engines `@playwright/mcp` can actually launch here.
//!
//! Two different things are called "installed", and conflating them is what
//! made `--browser firefox` fail right after the person installed Firefox:
//!
//! * **channels** (`chrome`, `msedge`) — Playwright launches the *application*
//!   on this machine, so `/Applications/Google Chrome.app` is the thing to
//!   look for;
//! * **own builds** (`chromium`, `firefox`, `webkit`) — Playwright launches a
//!   build from its own cache, pinned to one revision per release. A branded
//!   Firefox.app is irrelevant to it: the launch looks only at
//!   `<cache>/firefox-<revision>`, and a cache holding a *different* revision
//!   (`firefox-1553` when the pin wants `1549`) is as good as empty.
//!
//! The revision is read from playwright-core's own `browsers.json` — the file
//! Playwright itself consults — so this check cannot drift from the launch.

use std::path::{Path, PathBuf};

/// Does Playwright launch `engine` from its own cache (rather than from an
/// installed application)? Unknown names are treated as channels: a wrong
/// "install this build" instruction is worse than letting Playwright speak.
pub fn uses_own_build(engine: &str) -> bool {
    matches!(engine, "chromium" | "firefox" | "webkit")
}

/// `browsers.json` inside an install dir, relative to it.
const BROWSERS_JSON: [&str; 3] = ["node_modules", "playwright-core", "browsers.json"];

/// The cache revision the pinned server needs for `engine`, from its own
/// `browsers.json`. `None` when the file, the entry, or the field is missing —
/// callers then skip the check rather than invent a revision.
pub fn required_revision(install_dir: &Path, engine: &str) -> Option<String> {
    let path = BROWSERS_JSON
        .iter()
        .fold(install_dir.to_path_buf(), |p, c| p.join(c));
    let raw = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("browsers")?
        .as_array()?
        .iter()
        .find(|b| b.get("name").and_then(serde_json::Value::as_str) == Some(engine))?
        .get("revision")?
        .as_str()
        .map(str::to_owned)
}

/// The completed cache build for `engine` at `revision`, if it is there. An
/// interrupted download has no `INSTALLATION_COMPLETE` and does not count,
/// same rule as [`crate::chromium::installed_builds`].
pub fn build_dir(browsers: &Path, engine: &str, revision: &str) -> Option<PathBuf> {
    let dir = browsers.join(format!("{engine}-{revision}"));
    dir.join("INSTALLATION_COMPLETE").is_file().then_some(dir)
}

/// Whether the pinned server can launch `engine` from its cache: the revision
/// it wants is known AND that build is there. `None` means "cannot tell"
/// (no `browsers.json` yet, or no cache dir), which callers must not report as
/// missing.
pub fn own_build_ready(install_dir: &Path, browsers: Option<&Path>, engine: &str) -> Option<bool> {
    let revision = required_revision(install_dir, engine)?;
    let browsers = browsers?;
    Some(build_dir(browsers, engine, &revision).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned(dir: &Path, json: &str) {
        let p = BROWSERS_JSON
            .iter()
            .fold(dir.to_path_buf(), |p, c| p.join(c));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, json).unwrap();
    }

    const JSON: &str = r#"{"browsers":[{"name":"firefox","revision":"1549"},
        {"name":"chromium","revision":"1246"}]}"#;

    #[test]
    fn revision_comes_from_playwrights_own_manifest() {
        let d = tempfile::tempdir().unwrap();
        pinned(d.path(), JSON);
        assert_eq!(
            required_revision(d.path(), "firefox").as_deref(),
            Some("1549")
        );
        assert_eq!(required_revision(d.path(), "webkit"), None);
    }

    /// The reported bug: a cache with a NEWER firefox than the pin wants is
    /// not a usable install, because the launch looks for the exact revision.
    #[test]
    fn a_different_revision_is_not_ready() {
        let install = tempfile::tempdir().unwrap();
        pinned(install.path(), JSON);
        let cache = tempfile::tempdir().unwrap();
        let other = cache.path().join("firefox-1553");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("INSTALLATION_COMPLETE"), "").unwrap();
        assert_eq!(
            own_build_ready(install.path(), Some(cache.path()), "firefox"),
            Some(false)
        );
        let wanted = cache.path().join("firefox-1549");
        std::fs::create_dir_all(&wanted).unwrap();
        std::fs::write(wanted.join("INSTALLATION_COMPLETE"), "").unwrap();
        assert_eq!(
            own_build_ready(install.path(), Some(cache.path()), "firefox"),
            Some(true)
        );
    }

    /// An interrupted download must not read as installed.
    #[test]
    fn incomplete_build_is_not_ready() {
        let install = tempfile::tempdir().unwrap();
        pinned(install.path(), JSON);
        let cache = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cache.path().join("firefox-1549")).unwrap();
        assert_eq!(
            own_build_ready(install.path(), Some(cache.path()), "firefox"),
            Some(false)
        );
    }

    /// No manifest (server not installed yet) is "cannot tell", never "missing".
    #[test]
    fn unknown_without_manifest() {
        let install = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        assert_eq!(
            own_build_ready(install.path(), Some(cache.path()), "firefox"),
            None
        );
    }

    #[test]
    fn channels_do_not_use_a_cache_build() {
        assert!(uses_own_build("firefox"));
        assert!(uses_own_build("chromium"));
        assert!(!uses_own_build("chrome"));
        assert!(!uses_own_build("msedge"));
    }
}
