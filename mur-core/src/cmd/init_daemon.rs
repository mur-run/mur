//! Installs murmurd as a login-persistent service (launchd / systemd).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

mod service_home;
use service_home::ServiceHome;

/// launchd label of the murmurd job `install_launchd` registers.
pub(crate) const LAUNCHD_LABEL: &str = "run.mur.murmurd";
/// systemd user unit `install_systemd` registers.
pub(crate) const SYSTEMD_UNIT: &str = "murmurd.service";

/// A service manager that supervises murmurd (restarts it on exit).
///
/// When one is registered, the CLI must start/restart murmurd THROUGH it:
/// a directly spawned daemon grabs the singleton lock, the supervised job
/// then exits "already running" on every respawn (every `ThrottleInterval`,
/// forever), and the service manager no longer controls the live daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DaemonService {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Launchd,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Systemd,
}

impl std::fmt::Display for DaemonService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Launchd => "launchd",
            Self::Systemd => "systemd",
        })
    }
}

/// Who supervises the murmurd that serves the caller's MUR home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Supervision {
    /// No murmurd service is registered (or it is disabled and not running).
    None,
    /// A registered service runs murmurd for exactly the caller's MUR home:
    /// start/restart must go through it.
    Owns(DaemonService),
    /// A service is registered but its baked-in `MUR_HOME` names a different
    /// data root. Controlling it would act on the wrong daemon, and a direct
    /// daemon for the caller's home cannot contend with it (separate lock).
    Other {
        service: DaemonService,
        service_home: PathBuf,
    },
    /// A service is registered and live, but which MUR home it runs cannot be
    /// proven (`reason` says why). Neither routing through it nor starting a
    /// direct daemon is safe: one may act on another home's daemon, the other
    /// may fight the service for this home's lock.
    Unknown {
        service: DaemonService,
        reason: String,
    },
}

impl Supervision {
    /// Detect the supervision of the daemon for the caller's resolved MUR home.
    pub(crate) fn detect() -> Self {
        let Some(caller) = mur_common::home::try_mur_home() else {
            return Self::None;
        };
        let Some((service, home)) = DaemonService::registered() else {
            return Self::None;
        };
        let env_home = match home {
            ServiceHome::Known(h) => h,
            ServiceHome::Unknown(reason) => return Self::Unknown { service, reason },
        };
        match mur_common::home::resolve_from(env_home.map(Into::into), dirs::home_dir().as_deref())
        {
            Some(service_home) => Self::classify(service, service_home, &caller),
            None => Self::Unknown {
                service,
                reason: "the service sets no MUR_HOME and there is no home directory to \
                         resolve the default from"
                    .into(),
            },
        }
    }

    /// Pure ownership rule: the service owns the caller's daemon only when
    /// its effective data root is the caller's.
    fn classify(service: DaemonService, service_home: PathBuf, caller: &Path) -> Self {
        let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        if norm(&service_home) == norm(caller) {
            Self::Owns(service)
        } else {
            Self::Other {
                service,
                service_home,
            }
        }
    }
}

impl DaemonService {
    /// The service manager that has murmurd registered and live on this
    /// machine, plus the `MUR_HOME` it effectively runs with (see
    /// [`service_home`] for why that is not just the unit file's value).
    fn registered() -> Option<(Self, ServiceHome)> {
        #[cfg(target_os = "macos")]
        {
            let plist = dirs::home_dir()?
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist"));
            // Bytes, not `read_to_string`: a binary plist (e.g. after
            // `plutil -convert binary1`) is still a registered job and must
            // come back as `Unknown`, not as "no service".
            if let Ok(body) = std::fs::read(&plist)
                && quiet_success("launchctl", &["print", &launchd_target()])
            {
                let home =
                    service_home::launchd_service_home(&String::from_utf8_lossy(&body), || {
                        command_stdout("launchctl", &["getenv", mur_common::home::MUR_HOME_ENV])
                    });
                return Some((Self::Launchd, home));
            }
        }
        #[cfg(target_os = "linux")]
        {
            let show = command_stdout(
                "systemctl",
                &[
                    "--user",
                    "show",
                    "--property=LoadState,ActiveState,UnitFileState,MainPID,Environment,\
                     EnvironmentFiles,UnsetEnvironment",
                    SYSTEMD_UNIT,
                ],
            )?;
            if systemd_owns_unit(&show) {
                let home = service_home::systemd_service_home(
                    &show,
                    |pid| std::fs::read(format!("/proc/{pid}/environ")).ok(),
                    || command_stdout("systemctl", &["--user", "show-environment"]),
                );
                return Some((Self::Systemd, home));
            }
        }
        None
    }

    /// argv that starts the supervised daemon; with `restart`, the running
    /// instance is killed and replaced (so it picks up an upgraded binary).
    pub(crate) fn start_argv(self, restart: bool, launchd_target: &str) -> Vec<String> {
        let v: Vec<&str> = match (self, restart) {
            (Self::Launchd, true) => vec!["launchctl", "kickstart", "-k", launchd_target],
            (Self::Launchd, false) => vec!["launchctl", "kickstart", launchd_target],
            (Self::Systemd, true) => vec!["systemctl", "--user", "restart", SYSTEMD_UNIT],
            (Self::Systemd, false) => vec!["systemctl", "--user", "start", SYSTEMD_UNIT],
        };
        v.into_iter().map(String::from).collect()
    }

    /// Start (or with `restart`, restart) murmurd through the service manager.
    pub(crate) fn start(self, restart: bool) -> Result<()> {
        let argv = self.start_argv(restart, &launchd_target());
        let status = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .with_context(|| format!("could not run `{}`", argv.join(" ")))?;
        if !status.success() {
            bail!("`{}` exited with {status}", argv.join(" "));
        }
        Ok(())
    }
}

/// `gui/<uid>/<label>` — the launchd domain target for the murmurd job.
fn launchd_target() -> String {
    #[cfg(unix)]
    let uid = unsafe { libc::getuid() };
    #[cfg(not(unix))]
    let uid = 0;
    format!("gui/{uid}/{LAUNCHD_LABEL}")
}

/// Value of `key` in `systemctl show` output (`Key=value` lines).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn show_property<'a>(show: &'a str, key: &str) -> Option<&'a str> {
    show.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
        .map(str::trim)
}

/// Whether systemd currently supervises the unit described by `systemctl
/// show`. Enablement alone is the wrong test: a running unit can be
/// disabled without being stopped, and `Restart=always` still respawns it.
/// So a loaded unit counts when it is enabled OR active/activating (the
/// latter covers the `auto-restart` window between respawns).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_owns_unit(show: &str) -> bool {
    if show_property(show, "LoadState") != Some("loaded") {
        return false;
    }
    let enabled = show_property(show, "UnitFileState").is_some_and(|s| s.starts_with("enabled"));
    let live = matches!(
        show_property(show, "ActiveState"),
        Some("active" | "activating" | "reloading" | "deactivating")
    );
    enabled || live
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn split_systemd_words(s: &str) -> Vec<String> {
    let (mut words, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    words.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(cur);
    }
    words
}

/// `MUR_HOME` from a launchd plist's `EnvironmentVariables` (the XML form
/// `install_launchd` writes), XML entities undone.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_plist_mur_home(plist: &str) -> Option<PathBuf> {
    let env = plist.split("<key>EnvironmentVariables</key>").nth(1)?;
    let env = &env[..env.find("</dict>")?];
    let key = format!("<key>{}</key>", mur_common::home::MUR_HOME_ENV);
    let after = env.split(&key).nth(1)?.trim_start();
    let v = after.strip_prefix("<string>")?;
    let v = &v[..v.find("</string>")?];
    let v = v
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    (!v.is_empty()).then(|| PathBuf::from(v))
}

/// stdout of a successful `program args…` run (stderr discarded), else `None`.
#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn quiet_success(program: &str, args: &[&str]) -> bool {
    std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Returns true if installation succeeded, false if the platform is
/// unsupported (WSL, container, etc.) — caller prints a fallback message.
pub(crate) fn install_daemon_service(murmurd_path: &Path) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        install_launchd(murmurd_path)?;
        return Ok(true);
    }
    #[cfg(target_os = "linux")]
    {
        install_systemd(murmurd_path)?;
        return Ok(true);
    }
    #[allow(unreachable_code)]
    Ok(false)
}

#[cfg(target_os = "macos")]
fn install_launchd(murmurd_path: &Path) -> Result<()> {
    let label = LAUNCHD_LABEL;
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let agents_dir = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents_dir)?;

    let plist_path = agents_dir.join(format!("{label}.plist"));
    let log_path = mur_common::home::mur_home_or_err()?.join("murmurd.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>ThrottleInterval</key>
    <integer>5</integer>{env}
</dict>
</plist>
"#,
        bin = murmurd_path.display(),
        log = log_path.display(),
        env = launchd_env_block(mur_common::home::env_override().as_deref()),
    );
    std::fs::write(&plist_path, &plist)?;

    // Load/reload the agent (ignore errors — user may not have launchctl in PATH)
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .status();
    let _ = std::process::Command::new("launchctl")
        .args(["load", "-w", &plist_path.to_string_lossy()])
        .status();

    Ok(())
}

#[cfg(target_os = "linux")]
fn install_systemd(murmurd_path: &Path) -> Result<()> {
    let unit_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".config")
        .join("systemd")
        .join("user");
    std::fs::create_dir_all(&unit_dir)?;

    let unit_path = unit_dir.join(SYSTEMD_UNIT);
    let unit = format!(
        "[Unit]\nDescription=murmurd — mur pattern daemon\n\n\
         [Service]\nExecStart={bin}\n{env}Restart=always\nRestartSec=5\n\n\
         [Install]\nWantedBy=default.target\n",
        bin = murmurd_path.display(),
        env = systemd_env_line(mur_common::home::env_override().as_deref()),
    );
    std::fs::write(&unit_path, &unit)?;

    // Enable + start (ignore errors — systemd may not be running, e.g. in containers)
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_UNIT])
        .status();

    Ok(())
}

/// `EnvironmentVariables` entry carrying `MUR_HOME` into the launchd job, or
/// nothing when it is unset. launchd does not inherit the installing shell's
/// environment, so without this the daemon reads `~/.mur` while the CLI
/// writes `$MUR_HOME` (#1696).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchd_env_block(mur_home: Option<&Path>) -> String {
    let Some(h) = mur_home else {
        return String::new();
    };
    let v = h
        .display()
        .to_string()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "\n    <key>EnvironmentVariables</key>\n    <dict>\n        <key>{key}</key>\n        <string>{v}</string>\n    </dict>",
        key = mur_common::home::MUR_HOME_ENV,
    )
}

/// systemd `Environment=` line carrying `MUR_HOME`, or empty when unset.
/// Same reason as [`launchd_env_block`].
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn systemd_env_line(mur_home: Option<&Path>) -> String {
    let Some(h) = mur_home else {
        return String::new();
    };
    let v = h
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        "Environment=\"{key}={v}\"\n",
        key = mur_common::home::MUR_HOME_ENV
    )
}

/// Locate the murmurd binary next to the current mur executable.
pub(crate) fn murmurd_bin_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("murmurd")))
        .unwrap_or_else(|| PathBuf::from("murmurd"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_units_carry_mur_home_only_when_set() {
        assert_eq!(launchd_env_block(None), "");
        assert_eq!(systemd_env_line(None), "");
        let b = launchd_env_block(Some(Path::new("/data/a&b")));
        assert!(b.contains("<key>MUR_HOME</key>"), "{b}");
        assert!(b.contains("<string>/data/a&amp;b</string>"), "{b}");
        assert_eq!(
            systemd_env_line(Some(Path::new("/data/my mur"))),
            "Environment=\"MUR_HOME=/data/my mur\"\n"
        );
    }

    #[test]
    fn service_home_round_trips_through_installed_units() {
        let plist = format!(
            "<dict>{}\n</dict>",
            launchd_env_block(Some(Path::new("/data/a&b")))
        );
        assert_eq!(
            launchd_plist_mur_home(&plist),
            Some(PathBuf::from("/data/a&b"))
        );
        assert_eq!(
            launchd_plist_mur_home("<dict><key>Label</key></dict>"),
            None
        );

        let unit = format!(
            "[Service]\nExecStart=/bin/murmurd\n{}Restart=always\n",
            systemd_env_line(Some(Path::new("/data/my \"mur\"")))
        );
        assert_eq!(
            service_home::systemd_env_mur_home(&unit).as_deref(),
            Some("/data/my \"mur\"")
        );
        assert_eq!(
            service_home::systemd_env_mur_home("Environment=FOO=1 MUR_HOME=/x\n").as_deref(),
            Some("/x")
        );
        assert_eq!(
            service_home::systemd_env_mur_home("[Service]\nExecStart=/bin/murmurd\n"),
            None
        );
    }

    #[test]
    fn service_for_another_mur_home_is_not_routed_through() {
        let s = DaemonService::Launchd;
        assert_eq!(
            Supervision::classify(
                s,
                PathBuf::from("/nonexistent/u/.mur"),
                Path::new("/nonexistent/u/.mur/")
            ),
            Supervision::Owns(s)
        );
        assert_eq!(
            Supervision::classify(
                s,
                PathBuf::from("/nonexistent/u/.mur"),
                Path::new("/nonexistent/alt")
            ),
            Supervision::Other {
                service: s,
                service_home: PathBuf::from("/nonexistent/u/.mur")
            }
        );
    }

    #[test]
    fn systemd_ownership_is_liveness_or_enablement_not_enablement_alone() {
        let show = |load: &str, active: &str, file: &str| {
            format!(
                "LoadState={load}\nActiveState={active}\nUnitFileState={file}\nFragmentPath=/u\n"
            )
        };
        // Disabled while running: Restart=always still respawns it.
        assert!(systemd_owns_unit(&show("loaded", "active", "disabled")));
        // Between respawns (SubState=auto-restart).
        assert!(systemd_owns_unit(&show("loaded", "activating", "disabled")));
        assert!(systemd_owns_unit(&show("loaded", "inactive", "enabled")));
        assert!(systemd_owns_unit(&show(
            "loaded",
            "inactive",
            "enabled-runtime"
        )));
        assert!(!systemd_owns_unit(&show("loaded", "inactive", "disabled")));
        assert!(!systemd_owns_unit(&show("loaded", "failed", "disabled")));
        assert!(!systemd_owns_unit(&show("not-found", "inactive", "")));
    }

    #[test]
    fn service_start_argv_restarts_in_place() {
        let t = "gui/501/run.mur.murmurd";
        assert_eq!(
            DaemonService::Launchd.start_argv(true, t),
            ["launchctl", "kickstart", "-k", t]
        );
        assert_eq!(
            DaemonService::Launchd.start_argv(false, t),
            ["launchctl", "kickstart", t]
        );
        assert_eq!(
            DaemonService::Systemd.start_argv(true, t),
            ["systemctl", "--user", "restart", SYSTEMD_UNIT]
        );
        assert_eq!(
            DaemonService::Systemd.start_argv(false, t),
            ["systemctl", "--user", "start", SYSTEMD_UNIT]
        );
    }

    #[test]
    fn launchd_target_names_the_installed_label() {
        let t = launchd_target();
        assert!(
            t.starts_with("gui/") && t.ends_with("/run.mur.murmurd"),
            "{t}"
        );
    }

    #[test]
    fn murmurd_bin_path_returns_path() {
        // Should never panic and should produce a path ending in "murmurd"
        let p = murmurd_bin_path();
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        assert!(
            name.contains("murmurd"),
            "expected murmurd in path, got: {p:?}"
        );
    }

    #[test]
    fn murmurd_bin_path_does_not_panic() {
        // Test that murmurd_bin_path() can be called without panicking.
        let _ = murmurd_bin_path();
    }
}
