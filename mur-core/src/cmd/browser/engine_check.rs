//! Preflight for `mur browser auth --browser <engine>`: refuse with the exact
//! install command when Playwright cannot launch the chosen engine.
//!
//! Before this, two different "no firefox" messages chased each other. The
//! app-bundle scan (`select_browser`) sees `/Applications/Firefox.app` and says
//! yes; the launch sees only `<cache>/firefox-<pinned revision>` and dies with
//! Playwright's own "Executable doesn't exist" — so installing Firefox the
//! application, which is what the first message implied, changed nothing. The
//! check that decides is the one the launch uses, and it names the command that
//! actually fixes it.

use std::path::Path;

use mur_browser::{PLAYWRIGHT_MCP_PKG, engines};

/// The install command for one engine's Playwright build. Mirrors
/// `doctor::install_argv` (same package, same subcommand) minus
/// `--only-shell`, which exists for the headless Chromium case only: a headed
/// `auth` window needs the full build.
pub fn install_argv(engine: &str) -> Vec<String> {
    ["npx", "-y", PLAYWRIGHT_MCP_PKG, "install-browser", engine]
        .map(str::to_owned)
        .to_vec()
}

/// Message for an engine whose pinned build is missing from the cache. Says
/// which revision, so a cache holding a *different* one stops looking like a
/// working install.
fn missing_text(engine: &str, revision: Option<&str>, browsers: Option<&Path>) -> String {
    let rev = match revision {
        Some(r) => format!("{engine}-{r}"),
        None => engine.to_owned(),
    };
    let where_ = match browsers {
        Some(d) => format!(" in {}", d.display()),
        None => String::new(),
    };
    format!(
        "Playwright has no `{rev}` build{where_}, so it cannot launch {engine}. \
         Installing the {engine} application does not help: this launch only uses \
         Playwright's own build, pinned to that revision. Install it with:\n  {}",
        install_argv(engine).join(" ")
    )
}

/// `Err` only when the build is known to be absent. "Cannot tell" (no pinned
/// manifest, no cache dir) and channel engines (`chrome`, `msedge`, which use
/// the installed application) both pass: Playwright's own error is better than
/// a guess.
pub fn preflight(engine: &str, install_dir: &Path, browsers: Option<&Path>) -> anyhow::Result<()> {
    if !engines::uses_own_build(engine) {
        return Ok(());
    }
    if engines::own_build_ready(install_dir, browsers, engine) == Some(false) {
        anyhow::bail!(missing_text(
            engine,
            engines::required_revision(install_dir, engine).as_deref(),
            browsers,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{"browsers":[{"name":"firefox","revision":"1549"}]}"#;

    fn install_tree(dir: &Path) {
        let p = dir.join("node_modules/playwright-core/browsers.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, JSON).unwrap();
    }

    fn complete(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("INSTALLATION_COMPLETE"), "").unwrap();
    }

    /// The reported bug: a newer cached revision must fail here, naming the
    /// revision and the install command — not at launch with Playwright's
    /// "Executable doesn't exist".
    #[test]
    fn wrong_revision_fails_with_the_install_command() {
        let install = tempfile::tempdir().unwrap();
        install_tree(install.path());
        let cache = tempfile::tempdir().unwrap();
        complete(cache.path(), "firefox-1553");
        let err = preflight("firefox", install.path(), Some(cache.path())).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("firefox-1549"), "{text}");
        assert!(text.contains("install-browser firefox"), "{text}");
        // Must kill the wrong fix the old message implied.
        assert!(text.contains("application does not help"), "{text}");
    }

    #[test]
    fn pinned_revision_present_passes() {
        let install = tempfile::tempdir().unwrap();
        install_tree(install.path());
        let cache = tempfile::tempdir().unwrap();
        complete(cache.path(), "firefox-1549");
        preflight("firefox", install.path(), Some(cache.path())).unwrap();
    }

    /// `chrome` launches the installed application, so a cache miss is not a
    /// reason to refuse.
    #[test]
    fn channel_engines_are_never_refused() {
        let install = tempfile::tempdir().unwrap();
        install_tree(install.path());
        let cache = tempfile::tempdir().unwrap();
        preflight("chrome", install.path(), Some(cache.path())).unwrap();
        preflight("msedge", install.path(), None).unwrap();
    }

    /// No manifest: say nothing, let the launch speak.
    #[test]
    fn unknown_state_passes() {
        let install = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        preflight("firefox", install.path(), Some(cache.path())).unwrap();
    }

    /// Headed auth needs the full build, so `--only-shell` must not leak in.
    #[test]
    fn install_argv_is_not_shell_only() {
        let argv = install_argv("firefox");
        assert!(!argv.iter().any(|a| a == "--only-shell"), "{argv:?}");
        assert_eq!(argv.last().unwrap(), "firefox");
    }
}
