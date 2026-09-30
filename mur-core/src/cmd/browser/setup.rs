//! `mur browser setup`: install the Chromium replay launches, on a literal
//! `yes`, then prove it renders.
//!
//! Doctor stays read-only and prints the install command; setup runs that
//! same argv (`doctor::install_argv`) after printing it and asking. The
//! consent rule is shared with deep research (`cmd::consent`).
//!
//! Consent is a typed `yes` on a terminal, or the explicit `--yes` flag. The
//! flag exists because setup is unusable from inside murmur otherwise: there
//! is no TTY there, so the prompt has nothing to read and setup bails before
//! it can grant anything. `--yes` is not a silent mode — every step still
//! prints what it is about to do first — and it is not a default: an agent
//! reaches it only by spawning `mur`, which the HITL gate already put in
//! front of a human. Precedent: `grant_egress(…, yes)` in deep research.
//!
//! [`prepare`] covers everything up to the live test and is pure over its
//! inputs, so tests exec nothing. `Ok(())` means "run the live test next";
//! dispatch then calls `doctor::live_check`, whose result is the exit code.

use std::ffi::OsStr;
use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Result, bail};

use super::doctor::{Chromium, Probe, install_argv, install_hint, l1_check, usable_build};
use super::perms;

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

/// How consent will be obtained, and whether it can be at all.
///
/// One type rather than two bools because only three of the four
/// combinations are meaningful, and the fourth (`--yes` on a terminal) must
/// behave as pre-approved rather than prompt anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Ask on the terminal; only a literal `yes` proceeds.
    Ask,
    /// `--yes` was passed: consent already given, still print every step.
    Given,
    /// No terminal and no `--yes` — nothing may be asked, so nothing runs.
    Impossible,
}

impl Consent {
    /// `interactive` is `stdin.is_terminal()`; `yes` is the `--yes` flag.
    pub fn new(interactive: bool, yes: bool) -> Self {
        match (yes, interactive) {
            (true, _) => Self::Given,
            (false, true) => Self::Ask,
            (false, false) => Self::Impossible,
        }
    }

    fn given(self) -> bool {
        self == Self::Given
    }
}

/// Steps 1–5 of the setup flow: check, and install only on consent.
pub fn prepare(
    consent: Consent,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    path_var: &OsStr,
    browsers: Option<&Path>,
    probe: Probe<'_>,
    install: Installer<'_>,
) -> Result<()> {
    if consent == Consent::Impossible {
        bail!(
            "`setup` needs a terminal to ask for consent. Pass --yes to consent up front \
             (this is how it runs inside murmur), or run:\n  {}\n  mur browser doctor --live",
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
        "    download  ~96 MiB (headless shell + ffmpeg, the revision this pinned package wants)"
    )?;
    writeln!(
        output,
        "    note      npx may print a WARNING box about `npm install`; it does not apply here"
    )?;
    if !consent.given() && !crate::cmd::consent::literal_yes(input, output)? {
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

    match usable_build(dir) {
        Some(name) => {
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

/// Step 6: grant the spawn permissions a browser run needs.
///
/// The impure edge over [`perms`]: it resolves the agent, reads the profile
/// to see what is already granted, and applies each grant through the same
/// `mur agent perm` code path the printed commands would. A refusal leaves
/// the commands on screen and returns `Ok` — the install already happened
/// and the live test still tells the truth about rendering.
pub fn grant_perms(
    agent: Option<&str>,
    consent: Consent,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<()> {
    let mur_home = crate::cmd::agent::resolve_mur_home()?;
    let Some(agent) = agent
        .map(str::to_owned)
        .or_else(|| std::env::var("MUR_AGENT").ok())
    else {
        writeln!(
            output,
            "\nPermissions: no agent given (pass --agent <name> or set MUR_AGENT); skipping."
        )?;
        return Ok(());
    };
    // Case-insensitive like every other CLI agent lookup (rule 10), so the
    // grant lands on the profile the spoof check will compare against.
    let agent = crate::a2a_dial::canonicalize_agent_name(&mur_home, &agent);

    let (_, profile) = crate::cmd::agent::load_profile_for_edit(&agent)?;
    let spawn = &profile.entitlements.processes.spawn;
    let plan = perms::plan(&mur_home, &agent, &spawn.allowed, &spawn.allowed_dirs);
    let mut grant = |g: &perms::Grant| match g {
        perms::Grant::Binary(b) => crate::cmd::agent::cmd_perm_allow_spawn(&agent, b),
        perms::Grant::Dir(d) => crate::cmd::agent::cmd_perm_allow_spawn_dir(&agent, d),
    };
    perms::confirm_and_apply(&agent, &plan, consent.given(), input, output, &mut grant)?;
    Ok(())
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

    /// A completed build; a headless shell also gets its binary, since
    /// only a launchable shell counts.
    fn complete(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("INSTALLATION_COMPLETE"), "").unwrap();
        if name.starts_with("chromium_headless_shell-") {
            let exe = dir
                .join(name)
                .join("chrome-headless-shell-mac-arm64/chrome-headless-shell");
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(exe, "").unwrap();
        }
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
        run_with(interactive, false, answer, path_var, browsers, on_install)
    }

    fn run_with(
        interactive: bool,
        yes: bool,
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
            Consent::new(interactive, yes),
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
        let r = run(
            false,
            "yes\n",
            bin.path().as_os_str(),
            Some(browsers.path()),
            &never,
        );
        let err = format!("{:#}", r.result.unwrap_err());
        assert!(err.contains("needs a terminal"), "{err}");
        // The bail must name the way out, or murmur is a dead end.
        assert!(err.contains("--yes"), "{err}");
        assert!(err.contains(&install_hint()), "{err}");
        assert!(err.contains("mur browser doctor --live"), "{err}");
        assert_eq!(r.probes, 0, "bail before any check");
        assert!(r.out.is_empty(), "{}", r.out);
    }

    /// The murmur path: no TTY, but `--yes` carries the consent, so setup
    /// proceeds and installs instead of bailing.
    #[test]
    fn non_tty_with_yes_installs_without_prompting() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let dir = browsers.path().to_path_buf();
        let r = run_with(false, true, "", bin.path().as_os_str(), Some(&dir), &|| {
            complete(&dir, "chromium_headless_shell-1200");
            Ok(true)
        });
        assert!(r.result.is_ok(), "{:?} / {}", r.result, r.out);
        assert_eq!(r.installs.len(), 1, "installed exactly once");
        assert_eq!(r.installs[0], install_argv());
        // Consent is pre-given, not silent: the plan is still printed.
        assert!(!r.out.contains("Type 'yes'"), "{}", r.out);
        assert!(r.out.contains("Installing Chromium"), "{}", r.out);
    }

    /// `--yes` must not become a way to install on an empty answer without
    /// the flag: the same non-TTY run with `yes = false` still refuses.
    #[test]
    fn non_tty_without_yes_installs_nothing() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run_with(
            false,
            false,
            "yes\n",
            bin.path().as_os_str(),
            Some(browsers.path()),
            &never,
        );
        assert!(r.result.is_err());
        assert!(r.installs.is_empty());
    }

    #[test]
    fn missing_npx_stops_without_asking() {
        let empty = tempfile::tempdir().unwrap();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(
            true,
            "yes\n",
            empty.path().as_os_str(),
            Some(browsers.path()),
            &never,
        );
        assert!(r.result.is_err());
        assert!(r.out.contains("nodejs.org"), "{}", r.out);
        assert!(!r.out.contains("Type 'yes'"), "{}", r.out);
    }

    #[test]
    fn chromium_present_goes_to_live_without_asking() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        complete(browsers.path(), "chromium_headless_shell-1246");
        let r = run(
            true,
            "",
            bin.path().as_os_str(),
            Some(browsers.path()),
            &never,
        );
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
            let r = run(
                true,
                answer,
                bin.path().as_os_str(),
                Some(browsers.path()),
                &never,
            );
            assert!(r.result.is_err(), "{answer:?} must exit 1");
            assert!(r.out.contains("skipped"), "{answer:?}: {}", r.out);
        }
    }

    #[test]
    fn plan_is_printed_before_the_prompt() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(
            true,
            "no\n",
            bin.path().as_os_str(),
            Some(browsers.path()),
            &never,
        );
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
    fn plan_states_download_size_and_npx_warning() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(
            true,
            "no\n",
            bin.path().as_os_str(),
            Some(browsers.path()),
            &never,
        );
        let prompt = r.out.find("Type 'yes'").expect("prompted");
        let size = r.out.find("~96 MiB").expect("size printed");
        let warn = r.out.find("WARNING box").expect("npx warning explained");
        assert!(size < prompt && warn < prompt, "{}", r.out);
    }

    #[test]
    fn failing_installer_fails_setup() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(
            true,
            "yes\n",
            bin.path().as_os_str(),
            Some(browsers.path()),
            &|| Ok(false),
        );
        assert!(r.result.is_err());
        assert!(r.out.contains("✗ install command failed"), "{}", r.out);
    }

    #[test]
    fn installer_ok_but_no_build_names_the_dir() {
        let bin = path_with_npx();
        let browsers = tempfile::tempdir().unwrap();
        let r = run(
            true,
            "yes\n",
            bin.path().as_os_str(),
            Some(browsers.path()),
            &|| Ok(true),
        );
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
