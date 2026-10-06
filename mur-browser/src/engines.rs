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

/// Engine MUR picks for launches it starts itself. `@playwright/mcp` defaults
/// to the branded Google Chrome *application*, which is a different browser
/// from the one `mur browser setup` installs: it carries the person's real
/// profile, is often absent, and needs a spawn grant on `/Applications`.
/// Playwright's own Chromium build is the one MUR provisions and seals.
pub const DEFAULT_ENGINE: &str = "chromium";

/// `--browser=<DEFAULT_ENGINE>`, ready to push onto an argv.
pub const DEFAULT_ENGINE_ARG: &str = "--browser=chromium";

/// Flags through which a caller already picked the browser, so MUR must not
/// also push its default. `--cdp-endpoint`/`--connect-to` attach to a browser
/// that is already running, where an engine name is meaningless.
const ENGINE_FLAGS: [&str; 4] = ["--browser", "--cdp-endpoint", "--connect-to", "--device"];

/// Is `flag` present in `args`, as `--flag` or `--flag=value`?
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter()
        .any(|arg| arg == flag || arg.starts_with(&format!("{flag}=")))
}

/// Did the caller already choose an engine (or an attach target) in `args`?
/// Matches both `--browser x` and `--browser=x`.
pub fn engine_already_chosen(args: &[String]) -> bool {
    ENGINE_FLAGS.iter().any(|flag| has_flag(args, flag))
}

/// `@playwright/mcp`'s headless switch.
const HEADLESS_FLAG: &str = "--headless";

/// `@playwright/mcp`'s explicit browser binary.
const EXECUTABLE_FLAG: &str = "--executable-path";

/// The engine arguments to add to `args`: MUR's default Chromium unless the
/// caller already chose an engine or attach target. Chromium is named even
/// when its pinned build is known missing — leaving the engine unset is what
/// made `@playwright/mcp` fall back to the branded Chrome application, while
/// naming it makes Playwright report the missing build.
///
/// `mur browser setup` installs only the headless shell, so a cache without
/// the full build is the normal case, not an error: a headless launch is
/// then pointed at the shell, the way live and replay already launch. A
/// headed launch cannot use the shell, and an explicit `--executable-path`
/// is the caller's pick, so neither gets one.
pub fn default_engine_args(
    args: &[String],
    install_dir: &Path,
    browsers: Option<&Path>,
) -> Vec<String> {
    if engine_already_chosen(args) {
        return Vec::new();
    }
    let mut out = vec![DEFAULT_ENGINE_ARG.to_owned()];
    let needs_shell = has_flag(args, HEADLESS_FLAG)
        && !has_flag(args, EXECUTABLE_FLAG)
        && own_build_ready(install_dir, browsers, DEFAULT_ENGINE) == Some(false);
    if needs_shell {
        out.extend(crate::chromium::headless_exe_args(browsers));
    }
    out
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

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_owned()).collect()
    }

    #[test]
    fn an_explicit_engine_is_never_overridden() {
        assert!(engine_already_chosen(&s(&["--browser=firefox"])));
        assert!(engine_already_chosen(&s(&["--browser", "firefox"])));
        assert!(engine_already_chosen(&s(&["--cdp-endpoint=ws://x"])));
        assert!(engine_already_chosen(&s(&["--device", "iPhone 15"])));
        assert!(!engine_already_chosen(&s(&["--headless", "--isolated"])));
    }

    /// Live mode already asks for Chromium, so nothing is added twice.
    #[test]
    fn no_duplicate_when_caller_already_asked_for_chromium() {
        let install = tempfile::tempdir().unwrap();
        assert_eq!(
            default_engine_args(&s(&[DEFAULT_ENGINE_ARG]), install.path(), None),
            Vec::<String>::new()
        );
    }

    /// No manifest is "cannot tell": still ask for Chromium rather than
    /// silently falling back to the branded Chrome application.
    #[test]
    fn defaults_to_chromium_when_the_cache_is_unknown() {
        let install = tempfile::tempdir().unwrap();
        assert_eq!(
            default_engine_args(&s(&["--headless"]), install.path(), None),
            [DEFAULT_ENGINE_ARG.to_owned()]
        );
    }

    fn shell_only_cache() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
        let install = tempfile::tempdir().unwrap();
        pinned(install.path(), JSON);
        let cache = tempfile::tempdir().unwrap();
        let shell = cache.path().join("chromium_headless_shell-1246");
        let exe = shell.join("chrome-headless-shell-mac-arm64/chrome-headless-shell");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(shell.join("INSTALLATION_COMPLETE"), "").unwrap();
        std::fs::write(&exe, "").unwrap();
        (install, cache, exe)
    }

    /// #1731: `mur browser setup` installs `--only-shell`, so the cache holds
    /// the headless shell but no full `chromium-<rev>`. A headless launch must
    /// still name Chromium AND point at the shell, the way live and replay do;
    /// naming the engine alone would look for the absent full build.
    #[test]
    fn shell_only_headless_selects_chromium_on_the_shell() {
        let (install, cache, exe) = shell_only_cache();
        assert_eq!(
            default_engine_args(&s(&["--headless"]), install.path(), Some(cache.path())),
            [
                DEFAULT_ENGINE_ARG.to_owned(),
                format!("--executable-path={}", exe.display())
            ]
        );
    }

    /// A headed launch cannot run on the headless shell, so no executable is
    /// substituted — but the engine is still named, so Playwright reports the
    /// missing full build instead of quietly launching branded Chrome.
    #[test]
    fn shell_only_headed_names_chromium_without_the_shell() {
        let (install, cache, _) = shell_only_cache();
        assert_eq!(
            default_engine_args(&[], install.path(), Some(cache.path())),
            [DEFAULT_ENGINE_ARG.to_owned()]
        );
    }

    /// An explicit executable is the caller's pick; never add a second one.
    #[test]
    fn an_explicit_executable_is_kept() {
        let (install, cache, _) = shell_only_cache();
        assert_eq!(
            default_engine_args(
                &s(&["--headless", "--executable-path=/x"]),
                install.path(),
                Some(cache.path())
            ),
            [DEFAULT_ENGINE_ARG.to_owned()]
        );
    }

    /// A pinned build that is definitely absent still names Chromium. Leaving
    /// the engine unset is what made `@playwright/mcp` fall back to branded
    /// Chrome; with Chromium named, Playwright reports the missing build.
    #[test]
    fn a_missing_pinned_build_still_names_chromium() {
        let install = tempfile::tempdir().unwrap();
        pinned(
            install.path(),
            r#"{"browsers":[{"name":"chromium","revision":"1100"}]}"#,
        );
        let cache = tempfile::tempdir().unwrap();
        assert_eq!(
            default_engine_args(&s(&["--headless"]), install.path(), Some(cache.path())),
            [DEFAULT_ENGINE_ARG.to_owned()]
        );
    }
}
