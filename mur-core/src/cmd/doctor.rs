//! `mur agent doctor` — pre-flight prereq checks for export targets.
//!
//! Verifies that the host has every tool / library / credential that
//! `mur agent export --format <fmt>` will need, **without performing
//! the build**. Same logic is reused as a fail-fast step inside the
//! export pipeline (`mur-core/src/agent/export_gui.rs` phase 1) so a
//! green `mur agent doctor --format gui` predicts a successful export.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Missing,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    /// Detected version / details (None for missing / skipped).
    pub detail: Option<String>,
    /// Actionable hint when `status == Missing`.
    pub hint: Option<String>,
}

impl CheckResult {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            detail: Some(detail.into()),
            hint: None,
        }
    }
    fn missing(name: &str, hint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Missing,
            detail: None,
            hint: Some(hint.into()),
        }
    }
    fn skipped(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Skipped,
            detail: Some(reason.into()),
            hint: None,
        }
    }
}

/// Public entry point — invoked from `mur agent doctor`. Also re-used
/// internally by `agent::export_gui::pre_flight()`.
pub fn run(format: &str, json: bool) -> Result<()> {
    let results = checks_for(format);
    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        print_human(&results);
    }
    let any_missing = results.iter().any(|r| r.status == CheckStatus::Missing);
    if any_missing {
        anyhow::bail!("doctor found missing prerequisites");
    }
    Ok(())
}

/// Return the check list applicable to the given export format.
/// `format` is one of: `gui`, `bin`, `pkg`, `all`.
pub fn checks_for(format: &str) -> Vec<CheckResult> {
    let mut out = Vec::new();
    let want_gui = matches!(format, "gui" | "all");
    let want_build = matches!(format, "gui" | "bin" | "all");

    // Common toolchain
    out.push(check_command("cargo", &["--version"], "https://rustup.rs"));
    out.push(check_command(
        "rustc",
        &["--version"],
        "Install via rustup: https://rustup.rs",
    ));

    if want_build {
        // For both `bin` and `gui` we cargo-build the runtime.
        out.push(detect_target_arch());
    }

    if want_gui {
        // GUI-specific
        out.push(check_command(
            "node",
            &["--version"],
            "Install Node 20 LTS via nvm or your package manager",
        ));
        out.push(check_command("npm", &["--version"], "Bundled with Node"));
        out.push(check_tauri_cli());
        out.push(check_platform_libraries_for_gui());
        out.push(check_signing_credentials());
    }

    // pkg has no extra prereqs beyond the common toolchain.
    if format == "pkg" || format == "all" {
        // No-op: pkg just tars the agent home.
    }

    out
}

fn print_human(results: &[CheckResult]) {
    for r in results {
        let glyph = match r.status {
            CheckStatus::Ok => "✓",
            CheckStatus::Missing => "✗",
            CheckStatus::Skipped => "·",
        };
        match (&r.status, &r.detail, &r.hint) {
            (CheckStatus::Ok, Some(d), _) => println!("{glyph} {} ({})", r.name, d),
            (CheckStatus::Skipped, Some(d), _) => println!("{glyph} {} — {}", r.name, d),
            (CheckStatus::Missing, _, Some(h)) => println!("{glyph} {} → {}", r.name, h),
            _ => println!("{glyph} {}", r.name),
        }
    }
}

fn check_command(cmd: &str, args: &[&str], hint: &str) -> CheckResult {
    match Command::new(cmd).args(args).output() {
        Ok(out) if out.status.success() => {
            let detail = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            CheckResult::ok(cmd, detail)
        }
        _ => CheckResult::missing(cmd, hint),
    }
}

fn check_tauri_cli() -> CheckResult {
    match Command::new("cargo").args(["tauri", "--version"]).output() {
        Ok(out) if out.status.success() => {
            let detail = String::from_utf8_lossy(&out.stdout).trim().to_string();
            CheckResult::ok("tauri-cli", detail)
        }
        _ => CheckResult::missing(
            "tauri-cli",
            "cargo install tauri-cli --version '^2.0' --locked",
        ),
    }
}

fn detect_target_arch() -> CheckResult {
    // We can't easily query Rust's host triple at runtime without invoking
    // rustc again; use `cfg!` to derive the family for hint purposes.
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin (Apple Silicon)",
        ("macos", "x86_64") => {
            "x86_64-apple-darwin (Intel) — universal binary needs Apple Silicon host"
        }
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu (build on ubuntu-22.04 for widest compat)",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => {
            return CheckResult::skipped("host-target", format!("unrecognised host: {os} {arch}"));
        }
    };
    CheckResult::ok("host-target", triple)
}

#[cfg(target_os = "macos")]
fn check_platform_libraries_for_gui() -> CheckResult {
    // macOS has Cocoa / WebKit built-in. The one external thing is Xcode
    // Command Line Tools (clang, codesign).
    if Command::new("xcode-select")
        .arg("-p")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        CheckResult::ok("xcode-clt", "installed")
    } else {
        CheckResult::missing(
            "xcode-clt",
            "xcode-select --install   (required for codesign + clang)",
        )
    }
}

#[cfg(target_os = "linux")]
fn check_platform_libraries_for_gui() -> CheckResult {
    // pkg-config is the minimum to detect the rest; deeper checks can
    // be added per-distro later.
    if Command::new("pkg-config")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        CheckResult::ok(
            "linux-libs",
            "pkg-config present — assuming webkit2gtk + soup + appindicator (run apt list --installed | grep webkit2gtk to verify)",
        )
    } else {
        CheckResult::missing(
            "linux-libs",
            "sudo apt install -y libwebkit2gtk-4.1-dev libsoup-3.0-dev libayatana-appindicator3-dev pkg-config libfuse2",
        )
    }
}

#[cfg(target_os = "windows")]
fn check_platform_libraries_for_gui() -> CheckResult {
    // WebView2 is downloaded at install time on user machines; the
    // build-time concern is MSVC.
    if Command::new("cl.exe")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        CheckResult::ok("msvc", "cl.exe found")
    } else {
        CheckResult::missing(
            "msvc",
            "Install Visual Studio 2022 with the 'Desktop development with C++' workload",
        )
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn check_platform_libraries_for_gui() -> CheckResult {
    CheckResult::skipped(
        "platform-libs",
        format!("unrecognised platform: {}", std::env::consts::OS),
    )
}

fn check_signing_credentials() -> CheckResult {
    if cfg!(target_os = "macos") {
        let id = std::env::var("MUR_APPLE_DEVELOPER_ID").ok();
        let key = std::env::var("MUR_APPLE_NOTARY_KEY").ok();
        match (id, key) {
            (Some(_), Some(_)) => CheckResult::ok(
                "apple-signing",
                "MUR_APPLE_DEVELOPER_ID + MUR_APPLE_NOTARY_KEY set",
            ),
            _ => CheckResult::skipped(
                "apple-signing",
                "MUR_APPLE_DEVELOPER_ID / MUR_APPLE_NOTARY_KEY not set — exports will be unsigned (use --skip-notarize)",
            ),
        }
    } else if cfg!(target_os = "windows") {
        if std::env::var("MUR_WINDOWS_CERT_THUMBPRINT").is_ok() {
            CheckResult::ok("windows-signing", "MUR_WINDOWS_CERT_THUMBPRINT set")
        } else {
            CheckResult::skipped(
                "windows-signing",
                "MUR_WINDOWS_CERT_THUMBPRINT not set — exports will be unsigned",
            )
        }
    } else {
        CheckResult::skipped("signing", "no signing required on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkg_format_yields_minimal_checks() {
        let results = checks_for("pkg");
        assert!(results.iter().any(|r| r.name == "cargo"));
        assert!(!results.iter().any(|r| r.name == "node"));
    }

    #[test]
    fn gui_format_demands_node_and_npm() {
        let results = checks_for("gui");
        assert!(results.iter().any(|r| r.name == "node"));
        assert!(results.iter().any(|r| r.name == "npm"));
        assert!(results.iter().any(|r| r.name == "tauri-cli"));
    }

    #[test]
    fn host_target_is_detected_or_skipped() {
        let results = checks_for("all");
        let arch = results
            .iter()
            .find(|r| r.name == "host-target")
            .expect("host-target check should always run for `all`");
        assert!(matches!(
            arch.status,
            CheckStatus::Ok | CheckStatus::Skipped
        ));
    }
}
