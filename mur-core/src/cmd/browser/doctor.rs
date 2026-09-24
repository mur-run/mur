//! `mur browser doctor`: check what `record` / `replay` need before an agent
//! finds out mid-run.
//!
//! `mur browser status` lists profiles and runs but never looks at the
//! toolchain, so a machine without `npx` or without Playwright's Chromium
//! looked healthy until `replay` failed to spawn. This mirrors
//! `mur deep-research doctor`: read-only, prints the exact install command
//! for anything missing, and exits non-zero when something is.
//!
//! - L1 (default): `npx` is on PATH, `node` / `npx` run, and a completed
//!   Playwright Chromium build is in the browsers cache.
//! - L2 (`--live`): spawn the pinned `@playwright/mcp` headless (same flags
//!   as replay), navigate to a loopback JS-only page, and check the snapshot
//!   shows the script's output. This is the only level that proves the
//!   pinned package and its exact Chromium revision work together.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use mur_browser::replay::{StdioCaller, ToolCaller};
use serde_json::{Value, json};

use crate::cmd::deep_research::browser::{render_passed, serve_render_page};

/// Runs a version probe. `Ok(Some(stdout))` = exit 0, `Ok(None)` = non-zero.
/// Stdout is captured so the version lands on the ✓ line instead of loose
/// between the checks.
pub type Probe<'a> = &'a mut dyn FnMut(&[&str]) -> Result<Option<String>>;

/// Real probe: captures stdout, leaves stderr on the terminal.
pub fn system_probe(argv: &[&str]) -> Result<Option<String>> {
    let (prog, args) = argv.split_first().expect("argv is never empty");
    let out = std::process::Command::new(prog)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| anyhow::anyhow!("could not start `{prog}`: {e}"))?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// First non-blank line of a `--version` output, trimmed.
pub fn version_line(stdout: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

/// Browser-install command for the pinned MCP package. `install-browser` is
/// the package's own alias for `playwright install` against the
/// playwright-core it bundles, so it fetches the exact revision it launches.
pub fn install_hint() -> String {
    format!(
        "npx -y {} install-browser chromium",
        mur_browser::PLAYWRIGHT_MCP_PKG
    )
}

/// Headless launch flags, identical to replay's so `--live` tests the path
/// replay actually takes (bundled Chromium, not branded Chrome).
fn live_args() -> Vec<String> {
    ["--headless", "--isolated", "--browser=chromium"]
        .map(str::to_owned)
        .to_vec()
}

/// A cold `npx -y` may download the package and start Chromium for the
/// first time; allow for that, but never hang forever.
const LIVE_TIMEOUT: Duration = Duration::from_secs(90);

/// Where Playwright keeps its browsers. `PLAYWRIGHT_BROWSERS_PATH=0` means
/// "inside node_modules", which this check cannot see — returns `None`.
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

/// Completed builds with the given prefix (`chromium`, `chromium_headless_shell`),
/// newest revision first. A folder without `INSTALLATION_COMPLETE` is an
/// interrupted download and does not count.
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

/// L1 check. `path_var` is the PATH `npx` would be spawned with; `run`
/// executes version probes. Never installs anything.
pub fn doctor(
    output: &mut dyn Write,
    path_var: &OsStr,
    browsers: Option<&Path>,
    run: Probe<'_>,
) -> Result<()> {
    let mut problems = 0;

    writeln!(
        output,
        "Playwright MCP ({}):",
        mur_browser::PLAYWRIGHT_MCP_PKG
    )?;
    match mur_common::exec::resolve_command_in(path_var, "npx") {
        Ok(npx) => {
            writeln!(output, "  ✓ npx at {}", npx.display())?;
            for tool in ["node", "npx"] {
                match run(&[tool, "--version"]) {
                    Ok(Some(stdout)) => match version_line(stdout.as_bytes()) {
                        Some(v) => writeln!(output, "  ✓ {tool} runs ({v})")?,
                        None => writeln!(output, "  ✓ {tool} runs")?,
                    },
                    Ok(None) | Err(_) => {
                        problems += 1;
                        writeln!(output, "  ✗ `{tool} --version` failed")?;
                    }
                }
            }
        }
        Err(_) => {
            problems += 1;
            writeln!(output, "  ✗ npx not found on PATH")?;
            writeln!(
                output,
                "  install Node.js (it ships npx): https://nodejs.org"
            )?;
        }
    }

    writeln!(output, "Chromium for replay (headless):")?;
    match browsers {
        None => writeln!(
            output,
            "  ? PLAYWRIGHT_BROWSERS_PATH=0 keeps browsers inside node_modules; not checked (use --live)"
        )?,
        Some(dir) => {
            let shells = installed_builds(dir, "chromium_headless_shell");
            let full = installed_builds(dir, "chromium");
            match shells.first().or(full.first()) {
                Some((_, name)) => {
                    writeln!(output, "  ✓ {name} in {}", dir.display())?;
                    writeln!(
                        output,
                        "  (newest build found; --live confirms it is the one the pinned package wants)"
                    )?;
                }
                None => {
                    problems += 1;
                    writeln!(
                        output,
                        "  ✗ no completed Chromium build in {}",
                        dir.display()
                    )?;
                    writeln!(output, "  install with: {}", install_hint())?;
                }
            }
        }
    }

    if problems > 0 {
        bail!("mur browser is not ready ({problems} problem(s) above)");
    }
    Ok(())
}

/// Concatenate the `text` parts of an MCP tool result.
fn tool_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn ensure_ok(tool: &str, result: &Value) -> Result<()> {
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        bail!("{tool} failed: {}", tool_text(result));
    }
    Ok(())
}

/// L2 core, separated from process plumbing so it runs against a fake
/// caller: navigate, snapshot, and report whether the page's script ran.
pub async fn render_probe<C: ToolCaller + Send>(caller: &mut C, url: &str) -> Result<bool> {
    let nav = caller
        .call_tool("browser_navigate", json!({ "url": url }))
        .await?;
    ensure_ok("browser_navigate", &nav)?;
    let snap = caller.call_tool("browser_snapshot", json!({})).await?;
    ensure_ok("browser_snapshot", &snap)?;
    Ok(render_passed(&tool_text(&snap)))
}

/// L2: the real thing, on a loopback page. Spawns `npx`, so it only runs
/// behind `--live`.
pub async fn live_check(output: &mut dyn Write) -> Result<()> {
    writeln!(
        output,
        "Live test (headless Chromium via Playwright MCP, local JS-only page):"
    )?;
    let page = serve_render_page()?;
    let started = Instant::now();
    let mut child = mur_browser::proxy::playwright_command(&live_args())
        .spawn()
        .context("spawn Playwright MCP server (is `npx` on PATH?)")?;
    let stdin = child.stdin.take().context("child stdin")?;
    let stdout = child.stdout.take().context("child stdout")?;
    let url = page.url.clone();
    let outcome = tokio::time::timeout(LIVE_TIMEOUT, async move {
        let mut caller = StdioCaller::connect(stdin, stdout).await?;
        render_probe(&mut caller, &url).await
    })
    .await;
    let _ = child.kill().await;
    let served = page.finish();
    match outcome {
        Ok(Ok(true)) => {
            writeln!(
                output,
                "  ✓ rendered in {:.1}s",
                started.elapsed().as_secs_f64()
            )?;
            Ok(())
        }
        Ok(Ok(false)) => {
            writeln!(output, "  ✗ page loaded but its script did not run")?;
            bail!("live test failed")
        }
        Ok(Err(e)) => {
            writeln!(output, "  ✗ {e:#}")?;
            if !served {
                writeln!(
                    output,
                    "  the browser never reached the page; if Chromium is missing: {}",
                    install_hint()
                )?;
            }
            bail!("live test failed")
        }
        Err(_) => {
            writeln!(output, "  ✗ no answer within {}s", LIVE_TIMEOUT.as_secs())?;
            bail!("live test timed out")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("INSTALLATION_COMPLETE"), "").unwrap();
    }

    /// A PATH holding a placeholder `npx` file; the runner stands in for exec.
    fn path_with_npx() -> tempfile::TempDir {
        let bin = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "npx.exe" } else { "npx" };
        std::fs::write(bin.path().join(name), "").unwrap();
        bin
    }

    fn run_doctor(
        path_var: &OsStr,
        browsers: Option<&Path>,
        ok: bool,
    ) -> (Result<()>, String, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        let mut run = |argv: &[&str]| {
            calls.push(argv.join(" "));
            Ok(ok.then(|| format!("{}-ver\n", argv[0])))
        };
        let result = doctor(&mut out, path_var, browsers, &mut run);
        (result, String::from_utf8(out).unwrap(), calls)
    }

    #[test]
    fn missing_npx_fails_and_says_how_to_get_it() {
        let empty = tempfile::tempdir().unwrap();
        let browsers = tempfile::tempdir().unwrap();
        complete(browsers.path(), "chromium_headless_shell-1246");
        let (result, out, calls) =
            run_doctor(empty.path().as_os_str(), Some(browsers.path()), true);
        assert!(result.is_err(), "{out}");
        assert!(out.contains("✗ npx not found"), "{out}");
        assert!(out.contains("nodejs.org"), "{out}");
        assert!(calls.is_empty(), "no npx means nothing to run: {calls:?}");
    }

    #[test]
    fn healthy_machine_passes_and_only_runs_version_probes() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        complete(browsers.path(), "chromium_headless_shell-1246");
        let (result, out, calls) = run_doctor(bin.path().as_os_str(), Some(browsers.path()), true);
        result.unwrap();
        assert!(out.contains("✓ chromium_headless_shell-1246"), "{out}");
        assert!(!out.contains('✗'), "{out}");
        assert_eq!(calls, ["node --version", "npx --version"]);
        // The version sits on the ✓ line, not loose between the checks.
        assert!(out.contains("  ✓ node runs (node-ver)\n"), "{out}");
        assert!(out.contains("  ✓ npx runs (npx-ver)\n"), "{out}");
    }

    #[test]
    fn version_line_is_first_non_blank_line_trimmed() {
        assert_eq!(
            version_line(b"\n  v22.22.0  \nextra\n"),
            Some("v22.22.0".into())
        );
        assert_eq!(version_line(b"10.9.2\r\n"), Some("10.9.2".into()));
        assert_eq!(version_line(b"  \n\n"), None);
    }

    #[test]
    fn a_silent_but_successful_probe_still_passes_without_parens() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        complete(browsers.path(), "chromium_headless_shell-1246");
        let mut out = Vec::new();
        let mut run = |_: &[&str]| Ok(Some(String::new()));
        doctor(
            &mut out,
            bin.path().as_os_str(),
            Some(browsers.path()),
            &mut run,
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("  ✓ node runs\n"), "{out}");
    }

    #[test]
    fn a_failing_node_is_a_problem() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        complete(browsers.path(), "chromium-1246");
        let (result, out, _) = run_doctor(bin.path().as_os_str(), Some(browsers.path()), false);
        assert!(result.is_err());
        assert!(out.contains("✗ `node --version` failed"), "{out}");
    }

    #[test]
    fn interrupted_download_does_not_count_and_prints_pinned_install() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(browsers.path().join("chromium_headless_shell-1246")).unwrap();
        let (result, out, _) = run_doctor(bin.path().as_os_str(), Some(browsers.path()), true);
        assert!(result.is_err(), "{out}");
        assert!(out.contains("✗ no completed Chromium build"), "{out}");
        assert!(out.contains(&install_hint()), "{out}");
        assert!(install_hint().contains(mur_browser::PLAYWRIGHT_MCP_PKG));
    }

    #[test]
    fn newest_revision_wins_numerically() {
        let browsers = tempfile::tempdir().unwrap();
        for name in [
            "chromium-999",
            "chromium-1000",
            "chromium-1217",
            "chromium_headless_shell-5000",
            "ffmpeg-1011",
        ] {
            complete(browsers.path(), name);
        }
        let revs: Vec<u32> = installed_builds(browsers.path(), "chromium")
            .iter()
            .map(|b| b.0)
            .collect();
        assert_eq!(revs, [1217, 1000, 999]);
    }

    #[test]
    fn browsers_path_env_overrides_and_zero_means_unknown() {
        let home = Path::new("/h");
        let custom = |k: &str| (k == "PLAYWRIGHT_BROWSERS_PATH").then(|| OsString::from("/pw"));
        assert_eq!(
            browsers_dir(&custom, Some(home)),
            Some(PathBuf::from("/pw"))
        );
        let zero = |k: &str| (k == "PLAYWRIGHT_BROWSERS_PATH").then(|| OsString::from("0"));
        assert_eq!(browsers_dir(&zero, Some(home)), None);
        #[cfg(target_os = "macos")]
        assert_eq!(
            browsers_dir(&|_| None, Some(home)),
            Some(PathBuf::from("/h/Library/Caches/ms-playwright"))
        );
    }

    #[test]
    fn live_uses_the_same_bundled_chromium_as_replay() {
        assert!(live_args().contains(&"--browser=chromium".to_owned()));
        assert!(live_args().contains(&"--headless".to_owned()));
    }

    struct Fake {
        calls: Vec<String>,
        snapshot: &'static str,
        nav_error: bool,
    }

    impl ToolCaller for Fake {
        async fn call_tool(&mut self, name: &str, _args: Value) -> Result<Value> {
            self.calls.push(name.to_owned());
            let (text, err) = match name {
                "browser_navigate" => ("navigated", self.nav_error),
                _ => (self.snapshot, false),
            };
            Ok(json!({"content": [{"type": "text", "text": text}], "isError": err}))
        }
    }

    #[tokio::test]
    async fn probe_passes_only_when_the_script_ran() {
        let mut ran = Fake {
            calls: vec![],
            snapshot: "- generic: MUR-RENDER-42",
            nav_error: false,
        };
        assert!(render_probe(&mut ran, "http://127.0.0.1:1/").await.unwrap());
        assert_eq!(ran.calls, ["browser_navigate", "browser_snapshot"]);

        let mut raw = Fake {
            calls: vec![],
            snapshot: crate::cmd::deep_research::browser::RENDER_PAGE,
            nav_error: false,
        };
        assert!(!render_probe(&mut raw, "http://127.0.0.1:1/").await.unwrap());
    }

    #[tokio::test]
    async fn probe_surfaces_a_failed_navigation() {
        let mut fake = Fake {
            calls: vec![],
            snapshot: "",
            nav_error: true,
        };
        let err = render_probe(&mut fake, "http://127.0.0.1:1/")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("browser_navigate failed"), "{err}");
        assert_eq!(
            fake.calls,
            ["browser_navigate"],
            "no snapshot after a failed navigate"
        );
    }
}
