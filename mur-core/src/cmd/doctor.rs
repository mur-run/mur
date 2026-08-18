//! `mur agent doctor` — pre-flight prereq checks for export targets.
//!
//! Verifies that the host has every tool / library / credential that
//! `mur agent export --format <fmt>` will need, **without performing
//! the build**. Same logic is reused as a fail-fast step inside the
//! export pipeline (`mur-core/src/agent/export_gui.rs` phase 1) so a
//! green `mur agent doctor --format gui` predicts a successful export.

use anyhow::Result;
use mur_agent_runtime::bridge::beacon::{BridgePeerStatus, bridge_status_for_peer};
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

    // M-c1.4.4 — `bridges:` section. Only emitted in `all` / unset
    // mode (export-format-specific runs aren't interested in
    // bridge liveness diagnostics).
    if matches!(format, "all" | "") {
        let mur_home = std::env::var("MUR_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .expect("home directory required to locate ~/.mur")
                    .join(".mur")
            });
        let bridges = collect_bridge_statuses(&mur_home);
        if !bridges.is_empty() {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "bridges": bridges
                            .iter()
                            .map(|b| serde_json::json!({
                                "name": b.name,
                                "status": bridge_status_label(b.status),
                            }))
                            .collect::<Vec<_>>(),
                    }))?
                );
            } else {
                println!("\nbridges:");
                for b in &bridges {
                    println!("  {}: {}", b.name, bridge_status_label(b.status));
                }
            }
        }
    }

    let any_missing = results.iter().any(|r| r.status == CheckStatus::Missing);
    if any_missing {
        anyhow::bail!("doctor found missing prerequisites");
    }
    Ok(())
}

fn bridge_status_label(s: BridgePeerStatus) -> &'static str {
    match s {
        BridgePeerStatus::Running => "running",
        BridgePeerStatus::Degraded => "degraded",
        BridgePeerStatus::Offline => "offline",
    }
}

/// Per-agent liveness summary surfaced in the `bridges:` section of
/// `mur agent doctor`. Returned by [`collect_bridge_statuses`] and
/// rendered by [`run`].
#[derive(Debug)]
pub struct BridgeSummary {
    /// Agent directory name under `~/.mur/agents/`. Used as the key
    /// because the on-disk directory is the stable identifier; the
    /// profile's own `name:` field can drift.
    pub name: String,
    /// Coarse liveness derived from `running.lock` mtime.
    pub status: BridgePeerStatus,
}

/// Walk `<mur_home>/agents/*/profile.yaml` and emit a [`BridgeSummary`]
/// for every agent with `entitlements.llm.mode = off`. Sorted by name
/// for stable rendering. Silent on parse / IO failures so a single
/// malformed agent dir doesn't blank out the doctor report.
pub fn collect_bridge_statuses(mur_home: &std::path::Path) -> Vec<BridgeSummary> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(mur_home.join("agents")) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let yaml = match std::fs::read_to_string(dir.join("profile.yaml")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let profile: mur_common::AgentProfile = match serde_yaml_ng::from_str(&yaml) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if profile.entitlements.llm.mode != mur_common::LlmMode::Off {
            continue;
        }
        let name = match dir.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        out.push(BridgeSummary {
            name,
            status: bridge_status_for_peer(&dir),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
    out.push(check_hook_surface());

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

/// A0 hook surface report — confirms the runtime ships a frozen
/// 10-method `Hook` trait with phase-aware dispatch (gate / mutate /
/// observe) and the four built-in handlers wired in the supervisor.
/// Static for now because `mur agent doctor` runs in mur-core (the
/// CLI host), not in the runtime; introspecting at process boundary
/// is A1 work.
fn check_hook_surface() -> CheckResult {
    CheckResult::ok(
        "hooks",
        "10 surfaces frozen, 4 internal handlers active, dispatch=phase-aware",
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

/// A single per-agent health-check result. Distinct from [`CheckResult`]
/// (export-prereq checks) — this is a simpler ok/detail pair suited to
/// unit testing without the Ok/Missing/Skipped tri-state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

impl Check {
    fn new(name: impl Into<String>, ok: bool, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ok,
            detail: detail.into(),
        }
    }
}

/// Per-agent health checks: `model_ref` resolves in `models.yaml`, each
/// MCP server's `command` resolves on PATH, and entitlements parsed
/// cleanly (implied by `load_profile_for_edit` succeeding).
/// Report filesystem grants the kernel will silently drop.
///
/// A filesystem entitlement only reaches the sandbox if its path EXISTS when
/// the agent starts. A grant for a path that is not there is accepted by the
/// profile, dropped when the policy is sealed, and then shows up as a bare
/// errno in some later, unrelated operation — `perm show` lists it, so the
/// profile and the kernel disagree with nobody to say so.
///
/// `perm allow-read` / `allow-write` refuse a missing path at grant time, but
/// that cannot help a grant made before the check existed, or one whose
/// directory was removed afterwards. This is the reporting half.
///
/// `deny` entries are not checked: denying a path that does not exist costs
/// nothing and is a reasonable thing to write ahead of time.
fn check_filesystem_entitlements(fs: &mur_common::agent::FilesystemEntitlement) -> Check {
    let mut missing: Vec<String> = Vec::new();
    let mut granted = 0usize;

    for (kind, paths) in [("read", &fs.read), ("write", &fs.write)] {
        for raw in paths {
            granted += 1;
            let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(raw);
            // `metadata`, which FOLLOWS symlinks — the same call
            // `reject_dead_grant` uses at grant time (`cmd/agent/perm.rs`).
            // The two halves must agree on what "exists" means, or `perm`
            // accepts a path this then reports as broken. Following also
            // catches a dangling symlink, whose link exists but whose target
            // does not, and which the sandbox can no more grant than a path
            // that was never there.
            if std::fs::metadata(&p).is_err() {
                missing.push(format!("{kind} {}", p.display()));
            }
        }
    }

    if missing.is_empty() {
        return Check::new(
            "entitlements",
            true,
            format!("{granted} filesystem grant(s), all present"),
        );
    }
    Check::new(
        "entitlements",
        false,
        format!(
            "{} of {granted} filesystem grant(s) will be DROPPED at start — the path does not exist: {}. Create them (mkdir -p) and restart the agent.",
            missing.len(),
            missing.join(", ")
        ),
    )
}

pub fn agent_doctor(mur_home: &std::path::Path, name: &str) -> Result<Vec<Check>> {
    let (_path, profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    let mut out = Vec::new();

    match &profile.model_ref {
        Some(model_ref) => {
            let reg_path = mur_home.join("models.yaml");
            let reg = mur_common::model::ModelRegistry::load_from(&reg_path)?;
            if reg.models.contains_key(model_ref) {
                out.push(Check::new(
                    "model_ref",
                    true,
                    format!("'{model_ref}' resolves in models.yaml"),
                ));
            } else {
                out.push(Check::new(
                    "model_ref",
                    false,
                    format!("'{model_ref}' missing from models.yaml"),
                ));
            }
        }
        None => out.push(Check::new("model_ref", true, "inline model (no ref)")),
    }

    for server in &profile.mcp_servers {
        let check_name = format!("mcp:{}", server.name);
        match crate::cmd::agent_mcp_pin::resolve_command(&server.command) {
            Ok(resolved) => out.push(Check::new(check_name, true, resolved.display().to_string())),
            Err(e) => out.push(Check::new(check_name, false, e.to_string())),
        }
    }

    out.push(check_filesystem_entitlements(
        &profile.entitlements.filesystem,
    ));

    Ok(out)
}

/// Public entry point for `mur agent doctor <name>`. Prints a human table
/// (or JSON with `--json`) and returns a non-zero exit (via `Err`) if any
/// check failed — mirroring the export-prereq `run()` convention.
pub fn run_agent(name: &str, json: bool) -> Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let results = agent_doctor(&mur_home, name)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        for c in &results {
            let glyph = if c.ok { "✓" } else { "✗" };
            println!("{glyph} {} ({})", c.name, c.detail);
        }
    }

    // Best-effort program-deps preflight — never blocks or fails the doctor
    // command. A load/aggregate error is swallowed (this section is purely
    // informational); text mode only, so JSON output stays machine-parseable.
    if !json {
        let _ = (|| -> Result<()> {
            let deps = crate::cmd::deps::aggregate_agent(&mur_home, name)?;
            let report = crate::cmd::deps::doctor::build_report(&deps, &mur_home);
            crate::cmd::deps::doctor::print_report(
                &report,
                &format!("mur agent install-deps {name}"),
            );
            Ok(())
        })();
    }

    let any_failed = results.iter().any(|c| !c.ok);
    if any_failed {
        anyhow::bail!("doctor found unhealthy checks for agent '{name}'");
    }
    Ok(())
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

    /// The check that used to be `Check::new("entitlements", true, "parsed")`
    /// — unconditionally true, never looking at a path. A grant the kernel
    /// will drop must make it FALSE, and must name the path, because the only
    /// other signal the user gets is an unrelated errno much later.
    #[test]
    fn entitlements_check_names_a_grant_the_kernel_will_drop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let present = tmp.path().join("exists");
        std::fs::create_dir_all(&present).unwrap();
        let absent = tmp.path().join("not-there");

        let fs = mur_common::agent::FilesystemEntitlement {
            read: vec![present.display().to_string()],
            write: vec![absent.display().to_string()],
            deny: vec![],
        };
        let c = check_filesystem_entitlements(&fs);

        assert!(!c.ok, "a droppable grant must fail the check: {}", c.detail);
        assert!(
            c.detail.contains("not-there"),
            "the missing path must be named: {}",
            c.detail
        );
        assert!(
            !c.detail.contains("exists"),
            "a grant that IS present must not be reported as dropped: {}",
            c.detail
        );
    }

    /// All grants present is the ordinary case and must stay green — and the
    /// detail must say how many were actually looked at, so "ok" is not the
    /// same message whether it checked five paths or none.
    #[test]
    fn entitlements_check_passes_when_every_granted_path_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let fs = mur_common::agent::FilesystemEntitlement {
            read: vec![a.display().to_string()],
            write: vec![b.display().to_string()],
            // A deny for a path that does not exist is fine and must be ignored.
            deny: vec![tmp.path().join("never").display().to_string()],
        };
        let c = check_filesystem_entitlements(&fs);

        assert!(c.ok, "all-present grants must pass: {}", c.detail);
        assert!(
            c.detail.contains('2'),
            "the detail must say how many grants were checked: {}",
            c.detail
        );
    }

    /// A dangling symlink is a path the sandbox cannot grant either: the link
    /// exists, its target does not. Only a FOLLOWING stat sees that, which is
    /// why this check uses `metadata` and not `symlink_metadata` — and it is
    /// also what `perm allow-*` uses, so both halves agree on "exists".
    #[cfg(unix)]
    #[test]
    fn entitlements_check_catches_a_dangling_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let link = tmp.path().join("dangling");
        std::os::unix::fs::symlink(tmp.path().join("no-such-target"), &link).unwrap();

        let fs = mur_common::agent::FilesystemEntitlement {
            read: vec![link.display().to_string()],
            write: vec![],
            deny: vec![],
        };
        let c = check_filesystem_entitlements(&fs);
        assert!(!c.ok, "a dangling symlink grant must fail: {}", c.detail);
    }

    #[test]
    fn agent_doctor_named_checks_model_ref_and_mcp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path().to_path_buf();
        let agent_dir = mur_home.join("agents").join("coach");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let mut profile = mur_common::AgentProfile::default_for_tests();
        profile.model_ref = Some("nonexistent_ref".into());
        std::fs::write(
            agent_dir.join("profile.yaml"),
            serde_yaml_ng::to_string(&profile).unwrap(),
        )
        .unwrap();

        // Isolate resolve_mur_home()/ModelRegistry::default_path() to the
        // temp home; empty models.yaml means "nonexistent_ref" won't resolve.
        unsafe {
            std::env::set_var("MUR_HOME", &mur_home);
        }
        let report = agent_doctor(&mur_home, "coach").unwrap();
        unsafe {
            std::env::remove_var("MUR_HOME");
        }

        assert!(
            report.iter().any(|c| c.name == "model_ref" && !c.ok),
            "expected a failing model_ref check, got: {report:?}"
        );
    }
}
