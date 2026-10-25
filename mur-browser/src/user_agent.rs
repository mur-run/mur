//! A regular Chrome User-Agent for launches on the headless shell (#2099).
//!
//! The headless shell advertises `HeadlessChrome/<version>`, and sites that
//! filter on that token (CloudFront in front of chat.deepseek.com, for one)
//! answer `403` before any page loads. `@playwright/mcp` takes
//! `--user-agent`, which sets both the request header and
//! `navigator.userAgent`, so a shell launch passes the UA real Chrome of the
//! same major version sends.
//!
//! The version comes from the installed binary (`--version`), never a
//! constant, so it follows `mur browser setup` upgrades. The string uses
//! Chrome's reduced User-Agent format: the platform token and the minor
//! version are frozen by Chrome itself, which is what makes them constants
//! here rather than guesses about the host.

use std::path::Path;
use std::process::{Command, Stdio};

/// `@playwright/mcp` flag setting the browser context's User-Agent.
pub const USER_AGENT_FLAG: &str = "--user-agent";

/// Platform token reduced Chrome sends on macOS, whatever the real version
/// or CPU.
const PLATFORM_MAC: &str = "Macintosh; Intel Mac OS X 10_15_7";
/// Platform token reduced Chrome sends on Windows.
const PLATFORM_WINDOWS: &str = "Windows NT 10.0; Win64; x64";
/// Platform token reduced Chrome sends on Linux (and other unixes).
const PLATFORM_LINUX: &str = "X11; Linux x86_64";

/// The reduced platform token for the host OS.
fn platform_token() -> &'static str {
    if cfg!(target_os = "macos") {
        PLATFORM_MAC
    } else if cfg!(windows) {
        PLATFORM_WINDOWS
    } else {
        PLATFORM_LINUX
    }
}

/// Major version from `--version` output such as
/// `Google Chrome for Testing 154.0.8037.0` or `HeadlessChrome 120.0.6099.5`.
/// The last dotted-number word wins; anything else is `None`.
pub fn parse_major(version_output: &str) -> Option<u32> {
    version_output.split_whitespace().rev().find_map(|word| {
        let mut parts = word.split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        let rest: Vec<&str> = parts.collect();
        (!rest.is_empty() && rest.iter().all(|p| p.parse::<u32>().is_ok())).then_some(major)
    })
}

/// The User-Agent regular (non-headless) Chrome `major` sends on `platform`.
pub fn chrome_user_agent(platform: &str, major: u32) -> String {
    format!(
        "Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{major}.0.0.0 Safari/537.36"
    )
}

/// Major version of the Chromium binary at `exe`, asked via `--version`.
/// A binary that cannot run or prints no version yields `None`, and the
/// launch then keeps the package's own UA.
pub fn binary_major(exe: &Path) -> Option<u32> {
    let out = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_major(&String::from_utf8_lossy(&out.stdout))
}

/// `--user-agent=<regular Chrome UA>` for a launch on the shell at `exe`,
/// or nothing when its version cannot be read.
pub fn shell_user_agent_args(exe: &Path) -> Vec<String> {
    user_agent_args(binary_major(exe))
}

/// `--user-agent=<regular Chrome UA>` for a known major, nothing otherwise.
fn user_agent_args(major: Option<u32>) -> Vec<String> {
    major
        .map(|major| {
            vec![format!(
                "{USER_AGENT_FLAG}={}",
                chrome_user_agent(platform_token(), major)
            )]
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn major_is_read_from_version_output() {
        assert_eq!(
            parse_major("Google Chrome for Testing 154.0.8037.0\n"),
            Some(154)
        );
        assert_eq!(parse_major("HeadlessChrome 120.0.6099.5"), Some(120));
        assert_eq!(parse_major("Chromium 131.0.6778.33 snap"), Some(131));
    }

    #[test]
    fn no_version_means_no_major() {
        assert_eq!(parse_major(""), None);
        assert_eq!(parse_major("Google Chrome for Testing"), None);
        // A bare number is not a version.
        assert_eq!(parse_major("build 154"), None);
        assert_eq!(parse_major("154.x.1"), None);
    }

    #[test]
    fn user_agent_is_regular_chrome_not_headless() {
        let ua = chrome_user_agent(PLATFORM_MAC, 154);
        assert_eq!(
            ua,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/154.0.0.0 Safari/537.36"
        );
        assert!(!ua.contains("Headless"), "{ua}");
    }

    #[test]
    fn unreadable_binary_adds_nothing() {
        let d = tempfile::tempdir().unwrap();
        assert!(shell_user_agent_args(&d.path().join("missing")).is_empty());
    }

    #[test]
    fn known_major_becomes_the_user_agent_flag() {
        assert!(user_agent_args(None).is_empty());
        let args = user_agent_args(Some(154));
        assert_eq!(args.len(), 1, "{args:?}");
        assert!(
            args[0].starts_with("--user-agent=Mozilla/5.0 ("),
            "{args:?}"
        );
        assert!(args[0].contains(" Chrome/154.0.0.0 "), "{args:?}");
    }
}
