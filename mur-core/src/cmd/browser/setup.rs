//! `mur browser setup`: install the Chromium replay launches, on a literal
//! `yes`, then prove it renders.
//!
//! Doctor stays read-only and prints the install command; setup runs that
//! same argv (`doctor::install_argv`) after printing it and asking. The
//! consent rule is shared with deep research (`cmd::consent`). There is no
//! `--yes`: nothing downloads without a human typing `yes`.
//!
//! [`prepare`] covers everything up to the live test and is pure over its
//! inputs, so tests exec nothing. `Ok(())` means "run the live test next";
//! dispatch then calls `doctor::live_check`, whose result is the exit code.

use std::ffi::OsStr;
use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Result, bail};

use super::doctor::{Chromium, Probe, install_argv, install_hint, installed_builds, l1_check};

/// Runs the install argv. `Ok(true)` = exit 0.
pub type Installer<'a> = &'a mut dyn FnMut(&[String]) -> Result<bool>;

/// Real installer: unmodified `PATH` (same `npx` doctor reported and replay
/// spawns), inherited stdio so the download's progress is visible.
pub fn system_installer(argv: &[String]) -> Result<bool> {
    let (prog, args) = argv.split_first().expect("argv is never empty");
    let status = std::process::Command::new(prog)
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("could not start `{prog}`: {e}"))?;
    Ok(status.success())
}

/// Steps 1–5 of the setup flow: check, and install only on consent.
pub fn prepare(
    interactive: bool,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    path_var: &OsStr,
    browsers: Option<&Path>,
    probe: Probe<'_>,
    install: Installer<'_>,
) -> Result<()> {
    if !interactive {
        bail!(
            "`setup` is interactive; in scripts run:\n  {}\n  mur browser doctor --live",
            install_hint()
        );
    }

    let report = l1_check(output, path_var, browsers, probe)?;
    if !report.npx_ok {
        bail!("mur browser setup needs a working npx (see above)");
    }
    let dir = match (report.chromium, browsers) {
        (Chromium::Found(_), _) => return Ok(()),
        (Chromium::Unknown, _) | (Chromium::Missing, None) => {
            writeln!(
                output,
                "  (browsers dir unknown, so nothing to install here; the live test decides)"
            )?;
            return Ok(());
        }
        (Chromium::Missing, Some(dir)) => dir,
    };

    writeln!(output, "\nInstalling Chromium for replay does:")?;
    writeln!(output, "    run       {}", install_hint())?;
    writeln!(output, "    into      {}", dir.display())?;
    writeln!(
        output,
        "    (downloads the Chromium revision this pinned package launches)"
    )?;
    if !crate::cmd::consent::literal_yes(input, output)? {
        writeln!(output, "  skipped — nothing was downloaded.")?;
        bail!("Chromium not installed; mur browser is not ready");
    }

    match install(&install_argv()) {
        Ok(true) => {}
        Ok(false) => {
            writeln!(output, "  ✗ install command failed")?;
            bail!("Chromium install failed");
        }
        Err(e) => {
            writeln!(output, "  ✗ install command failed: {e:#}")?;
            bail!("Chromium install failed");
        }
    }

    let found = installed_builds(dir, "chromium_headless_shell")
        .into_iter()
        .chain(installed_builds(dir, "chromium"))
        .next();
    match found {
        Some((_, name)) => {
            writeln!(output, "  ✓ {name} in {}", dir.display())?;
            Ok(())
        }
        None => {
            writeln!(
                output,
                "  ✗ still no completed Chromium build in {}",
                dir.display()
            )?;
            bail!("Chromium install finished but no build appeared");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_with_npx() -> tempfile::TempDir {
        let bin = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "npx.exe" } else { "npx" };
        std::fs::write(bin.path().join(name), "").unwrap();
        bin
    }

    fn complete(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("INSTALLATION_COMPLETE"), "").unwrap();
    }

    struct Run {
        result: Result<()>,
        out: String,
        probes: usize,
        installs: Vec<Vec<String>>,
    }

    /// `on_install` stands in for the download: it gets the browsers dir and
    /// returns the installer's exit status.
    fn run(
        interactive: bool,
        answer: &str,
        path_var: &OsStr,
        browsers: Option<&Path>,
        on_install: &dyn Fn() -> Result<bool>,
    ) -> Run {
        let mut out = Vec::new();
        let mut probes = 0;
        let mut installs = Vec::new();
        let mut probe = |argv: &[&str]| {
            probes += 1;
            Ok(Some(format!("{}-ver\n", argv[0])))
        };
        let mut install = |argv: &[String]| {
            installs.push(argv.to_vec());
            on_install()
        };
        let result = prepare(
            interactive,
            &mut answer.as_bytes(),
            &mut out,
            path_var,
            browsers,
            &mut probe,
            &mut install,
        );
        Run {
            result,
            out: String::from_utf8(out).unwrap(),
            probes,
            installs,
        }
    }

    fn never() -> Result<bool> {
        panic!("installer must not run")
    }

    #[test]
    fn non_tty_bails_with_both_commands_and_runs_nothing() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(false, "yes\n", bin.path().as_os_str(), Some(browsers.path()), &never);
        let err = format!("{:#}", r.result.unwrap_err());
        assert!(err.contains("`setup` is interactive"), "{err}");
        assert!(err.contains(&install_hint()), "{err}");
        assert!(err.contains("mur browser doctor --live"), "{err}");
        assert_eq!(r.probes, 0, "bail before any check");
        assert!(r.out.is_empty(), "{}", r.out);
    }

    #[test]
    fn missing_npx_stops_without_asking() {
        let empty = tempfile::tempdir().unwrap();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(true, "yes\n", empty.path().as_os_str(), Some(browsers.path()), &never);
        assert!(r.result.is_err());
        assert!(r.out.contains("nodejs.org"), "{}", r.out);
        assert!(!r.out.contains("Type 'yes'"), "{}", r.out);
    }

    #[test]
    fn chromium_present_goes_to_live_without_asking() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        complete(browsers.path(), "chromium_headless_shell-1246");
        let r = run(true, "", bin.path().as_os_str(), Some(browsers.path()), &never);
        r.result.unwrap();
        assert!(!r.out.contains("Type 'yes'"), "{}", r.out);
    }

    #[test]
    fn unknown_browsers_dir_never_installs() {
        let bin = path_with_npx();
        let r = run(true, "yes\n", bin.path().as_os_str(), None, &never);
        r.result.unwrap();
        assert!(!r.out.contains("Type 'yes'"), "{}", r.out);
        assert!(r.out.contains("live test"), "says why it skips: {}", r.out);
    }

    #[test]
    fn yes_runs_exactly_the_doctor_argv_then_goes_live() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let dir = browsers.path().to_owned();
        let r = run(true, "yes\n", bin.path().as_os_str(), Some(&dir), &|| {
            complete(&dir, "chromium_headless_shell-1246");
            Ok(true)
        });
        r.result.unwrap();
        assert_eq!(r.installs, [install_argv()]);
        assert!(
            r.out.contains(&format!(
                "  ✓ chromium_headless_shell-1246 in {}",
                dir.display()
            )),
            "{}",
            r.out
        );
    }

    #[test]
    fn anything_but_yes_downloads_nothing_and_fails() {
        let bin = path_with_npx();
        for answer in ["y\n", "Y\n", "\n", ""] {
            let browsers = tempfile::tempdir().unwrap();
            let r = run(true, answer, bin.path().as_os_str(), Some(browsers.path()), &never);
            assert!(r.result.is_err(), "{answer:?} must exit 1");
            assert!(r.out.contains("skipped"), "{answer:?}: {}", r.out);
        }
    }

    #[test]
    fn plan_is_printed_before_the_prompt() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(true, "no\n", bin.path().as_os_str(), Some(browsers.path()), &never);
        let prompt = r.out.find("Type 'yes'").expect("prompted");
        let cmd = r.out.find(&install_hint()).expect("full argv printed");
        let dir = r
            .out
            .rfind(&browsers.path().display().to_string())
            .expect("dir printed");
        assert!(cmd < prompt && dir < prompt, "{}", r.out);
        // The plan's own line, not only doctor's hint above it.
        assert!(
            r.out.contains(&format!("    run       {}", install_hint())),
            "{}",
            r.out
        );
    }

    #[test]
    fn failing_installer_fails_setup() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(true, "yes\n", bin.path().as_os_str(), Some(browsers.path()), &|| {
            Ok(false)
        });
        assert!(r.result.is_err());
        assert!(r.out.contains("✗ install command failed"), "{}", r.out);
    }

    #[test]
    fn installer_ok_but_no_build_names_the_dir() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(true, "yes\n", bin.path().as_os_str(), Some(browsers.path()), &|| {
            Ok(true)
        });
        assert!(r.result.is_err());
        assert!(
            r.out.contains(&format!(
                "✗ still no completed Chromium build in {}",
                browsers.path().display()
            )),
            "{}",
            r.out
        );
    }
}
