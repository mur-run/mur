//! Preflight for `mur browser auth --browser <engine>` and headed
//! `mur browser record`: refuse with the exact install command when
//! Playwright cannot launch the chosen engine.
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

/// Did the caller pass `flag`, as `--flag` or `--flag=value`?
fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter()
        .any(|a| a == flag || a.starts_with(&format!("{flag}=")))
}

/// Message for a headed record on a cache without the full Chromium build.
/// Setups before the full build was added left only the headless shell, so
/// this is the common case on older caches, and Playwright's bare
/// "Executable doesn't exist" says neither why nor what to do. Both fixes are named: install the full build,
/// or record headless so the shell serves it.
fn headed_text(run: &str, revision: Option<&str>, browsers: Option<&Path>) -> String {
    let engine = engines::DEFAULT_ENGINE;
    format!(
        "{}\n\nOnly the headless shell is installed (what `mur browser setup` used to \
         fetch), and it cannot open a visible window. Re-running `mur browser setup` \
         adds the full build. Or record headless instead:\n  mur browser record --run {run} -- --headless",
        missing_text(engine, revision, browsers)
    )
}

/// Preflight for `mur browser record`, given the argv about to reach
/// `@playwright/mcp`. Refuses only a headed launch of MUR's default Chromium
/// whose full build is known absent. A headless launch (the shell serves it),
/// a caller-chosen engine, attach target or `--executable-path`, and an
/// unknown cache state all pass and let the launch speak.
pub fn record_preflight(
    run: &str,
    args: &[String],
    install_dir: &Path,
    browsers: Option<&Path>,
) -> anyhow::Result<()> {
    let ours = !engines::engine_already_chosen(args) && !has_flag(args, "--executable-path");
    if !ours || has_flag(args, "--headless") {
        return Ok(());
    }
    let engine = engines::DEFAULT_ENGINE;
    if engines::own_build_ready(install_dir, browsers, engine) == Some(false) {
        anyhow::bail!(headed_text(
            run,
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

    const CHROMIUM_JSON: &str = r#"{"browsers":[{"name":"chromium","revision":"1200"}]}"#;

    fn chromium_install(dir: &Path) {
        let p = dir.join("node_modules/playwright-core/browsers.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, CHROMIUM_JSON).unwrap();
    }

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    /// #1731 follow-up: a headed record on a shell-only cache must say why
    /// and name both fixes, instead of surfacing Playwright's bare
    /// "Executable doesn't exist".
    #[test]
    fn headed_record_on_shell_only_cache_names_both_fixes() {
        let install = tempfile::tempdir().unwrap();
        chromium_install(install.path());
        let cache = tempfile::tempdir().unwrap();
        complete(cache.path(), "chromium_headless_shell-1200");
        let err = record_preflight("demo", &[], install.path(), Some(cache.path())).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("chromium-1200"), "{text}");
        assert!(text.contains("headless shell"), "{text}");
        assert!(text.contains("install-browser chromium"), "{text}");
        assert!(!text.contains("--only-shell"), "{text}");
        assert!(text.contains("--run demo -- --headless"), "{text}");
    }

    #[test]
    fn record_preflight_passes_when_headless_chosen_or_ready() {
        let install = tempfile::tempdir().unwrap();
        chromium_install(install.path());
        let cache = tempfile::tempdir().unwrap();
        let p = Some(cache.path());
        // Headless: the shell serves it.
        record_preflight("r", &argv(&["--headless"]), install.path(), p).unwrap();
        // Caller picked the engine or an attach target: theirs to answer for.
        record_preflight("r", &argv(&["--browser=chrome"]), install.path(), p).unwrap();
        record_preflight("r", &argv(&["--cdp-endpoint", "x"]), install.path(), p).unwrap();
        // Unknown state: let the launch speak.
        let empty = tempfile::tempdir().unwrap();
        record_preflight("r", &[], empty.path(), p).unwrap();
        // Full build present.
        complete(cache.path(), "chromium-1200");
        record_preflight("r", &[], install.path(), p).unwrap();
    }

    /// Headed auth needs the full build, so `--only-shell` must not leak in.
    #[test]
    fn install_argv_is_not_shell_only() {
        let argv = install_argv("firefox");
        assert!(!argv.iter().any(|a| a == "--only-shell"), "{argv:?}");
        assert_eq!(argv.last().unwrap(), "firefox");
    }
}
