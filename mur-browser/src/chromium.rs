//! Where Playwright's Chromium builds live, and which binary headless
//! launches should use.
//!
//! `@playwright/mcp` launches full Chrome for Testing even with `--headless`,
//! so a cache holding only `chromium_headless_shell-*` failed at
//! `browser_navigate` while doctor reported it ready. Headless launches
//! (replay, `doctor --live`) now pass the shell via `--executable-path`
//! when one is installed, and fall back to the package default (full
//! Chromium) when it is not. Headed launches (`auth`) never use the shell:
//! it cannot open a window.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Where Playwright keeps its browsers. `PLAYWRIGHT_BROWSERS_PATH=0` means
/// "inside node_modules", which this cannot see — returns `None`.
pub fn browsers_dir(
    env: &dyn Fn(&str) -> Option<OsString>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(custom) = env("PLAYWRIGHT_BROWSERS_PATH").filter(|v| !v.is_empty()) {
        return (custom != "0").then(|| PathBuf::from(custom));
    }
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Caches/ms-playwright"));
    }
    if cfg!(windows) {
        return env("LOCALAPPDATA").map(|d| PathBuf::from(d).join("ms-playwright"));
    }
    env("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".cache")))
        .map(|d| d.join("ms-playwright"))
}

/// Folder prefix of the headless shell build.
pub const SHELL_PREFIX: &str = "chromium_headless_shell";
/// Folder prefix of the full Chromium (Chrome for Testing) build.
pub const FULL_PREFIX: &str = "chromium";

/// Completed builds with the given prefix, newest revision first. A folder
/// without `INSTALLATION_COMPLETE` is an interrupted download and does not
/// count.
pub fn installed_builds(dir: &Path, prefix: &str) -> Vec<(u32, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let lead = format!("{prefix}-");
    let mut builds: Vec<(u32, String)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let rev = name.strip_prefix(&lead)?.parse::<u32>().ok()?;
            e.path()
                .join("INSTALLATION_COMPLETE")
                .is_file()
                .then_some((rev, name))
        })
        .collect();
    builds.sort_by_key(|b| std::cmp::Reverse(b.0));
    builds
}

/// Executable paths inside a `chromium_headless_shell-*` folder, across the
/// layouts Playwright ships: Chrome-for-Testing zips per platform, and its
/// own `chrome-linux` build (linux arm64, older revisions).
const SHELL_EXES: &[&str] = &[
    "chrome-headless-shell-mac-arm64/chrome-headless-shell",
    "chrome-headless-shell-mac-x64/chrome-headless-shell",
    "chrome-headless-shell-linux64/chrome-headless-shell",
    "chrome-headless-shell-linux-arm64/chrome-headless-shell",
    "chrome-linux/headless_shell",
    "chrome-headless-shell-win64/chrome-headless-shell.exe",
    "chrome-win/headless_shell.exe",
];

/// The headless shell binary of the newest completed build in `dir`, if any.
pub fn headless_shell_exe(dir: &Path) -> Option<PathBuf> {
    installed_builds(dir, SHELL_PREFIX)
        .into_iter()
        .find_map(|(_, name)| {
            let build = dir.join(name);
            SHELL_EXES
                .iter()
                .map(|rel| build.join(rel))
                .find(|p| p.is_file())
        })
}

/// `--executable-path=<shell>` for a headless launch, or nothing (the
/// package then launches full Chromium, as before).
pub fn headless_exe_args(browsers: Option<&Path>) -> Vec<String> {
    browsers
        .and_then(headless_shell_exe)
        .map(|exe| vec![format!("--executable-path={}", exe.display())])
        .unwrap_or_default()
}

/// The browsers dir for this process's environment.
pub fn system_browsers_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from);
    browsers_dir(&|k: &str| std::env::var_os(k), home.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(dir: &Path, name: &str) -> PathBuf {
        let build = dir.join(name);
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("INSTALLATION_COMPLETE"), "").unwrap();
        build
    }

    fn exe(build: &Path, rel: &str) -> PathBuf {
        let p = build.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "").unwrap();
        p
    }

    #[test]
    fn shell_found_gives_executable_path() {
        let d = tempfile::tempdir().unwrap();
        let b = complete(d.path(), "chromium_headless_shell-1246");
        let p = exe(&b, "chrome-headless-shell-mac-arm64/chrome-headless-shell");
        assert_eq!(headless_shell_exe(d.path()), Some(p.clone()));
        assert_eq!(
            headless_exe_args(Some(d.path())),
            [format!("--executable-path={}", p.display())]
        );
    }

    #[test]
    fn full_only_falls_back_to_package_default() {
        let d = tempfile::tempdir().unwrap();
        let b = complete(d.path(), "chromium-1246");
        exe(&b, "chrome-mac-arm64/Google Chrome for Testing");
        assert_eq!(headless_shell_exe(d.path()), None);
        assert!(headless_exe_args(Some(d.path())).is_empty());
    }

    #[test]
    fn nothing_or_unknown_dir_adds_no_args() {
        let d = tempfile::tempdir().unwrap();
        assert!(headless_exe_args(Some(d.path())).is_empty());
        assert!(headless_exe_args(None).is_empty());
    }

    #[test]
    fn incomplete_or_exe_less_shell_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        // interrupted download: binary present, no INSTALLATION_COMPLETE
        let partial = d.path().join("chromium_headless_shell-1300");
        exe(
            &partial,
            "chrome-headless-shell-mac-arm64/chrome-headless-shell",
        );
        // complete marker but unknown layout
        complete(d.path(), "chromium_headless_shell-1250");
        assert_eq!(headless_shell_exe(d.path()), None);
    }

    #[test]
    fn newest_usable_shell_wins() {
        let d = tempfile::tempdir().unwrap();
        let old = complete(d.path(), "chromium_headless_shell-1200");
        exe(&old, "chrome-linux/headless_shell");
        let new = complete(d.path(), "chromium_headless_shell-1246");
        let p = exe(&new, "chrome-headless-shell-linux64/chrome-headless-shell");
        assert_eq!(headless_shell_exe(d.path()), Some(p));
    }

    #[test]
    fn browsers_path_zero_is_unknown() {
        let env = |k: &str| (k == "PLAYWRIGHT_BROWSERS_PATH").then(|| OsString::from("0"));
        assert_eq!(browsers_dir(&env, Some(Path::new("/h"))), None);
        let env = |k: &str| (k == "PLAYWRIGHT_BROWSERS_PATH").then(|| OsString::from("/x"));
        assert_eq!(browsers_dir(&env, None), Some(PathBuf::from("/x")));
    }
}
