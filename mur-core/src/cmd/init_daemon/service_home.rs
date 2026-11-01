//! Which MUR home a registered murmurd service actually runs with.
//!
//! The answer has to come from the service manager's *effective* view, not
//! from re-parsing the unit file `mur init` wrote: systemd layers drop-ins
//! and `EnvironmentFile=` over the fragment, and every unit inherits the user
//! manager's own environment (`environment.d`, `systemctl --user
//! set-environment`). When that view cannot be established the answer is
//! [`ServiceHome::Unknown`] — never a silent fallback to the default home,
//! because guessing wrong either kills another home's daemon or starts an
//! unsupervised one that fights the service for the lock.

use std::path::{Path, PathBuf};

use mur_common::home::MUR_HOME_ENV;

use super::{command_stdout, show_property, split_systemd_words};

/// Where per-user launchd job plists live, relative to the home directory.
pub(crate) const LAUNCH_AGENTS_DIR: &str = "Library/LaunchAgents";

/// `systemctl show` properties [`systemd_service_home`] needs, plus the
/// ownership ones [`super::systemd_owns_unit`] reads.
const SYSTEMD_SHOW_PROPERTIES: &str = "--property=LoadState,ActiveState,UnitFileState,MainPID,\
                                       Environment,EnvironmentFiles,UnsetEnvironment";

/// The `MUR_HOME` a murmurd service runs with, as far as it can be proven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServiceHome {
    /// The service's `MUR_HOME` (`None`: unset or empty, i.e. the default home).
    Known(Option<PathBuf>),
    /// Ownership cannot be established; the reason is shown to the user.
    Unknown(String),
}

/// The data root a service with home `home` uses: its `MUR_HOME`, else the
/// default home. `Err` carries the reason ownership cannot be proven.
pub(crate) fn resolve(home: ServiceHome) -> Result<PathBuf, String> {
    match home {
        ServiceHome::Unknown(reason) => Err(reason),
        ServiceHome::Known(h) => {
            mur_common::home::resolve_from(h.map(Into::into), dirs::home_dir().as_deref())
                .ok_or_else(|| {
                    "the service sets no MUR_HOME and there is no home directory to resolve \
                     the default from"
                        .into()
                })
        }
    }
}

/// The data root of the service whose descriptor is `unit` (a launchd plist
/// or a systemd user unit file), as [`resolve`] answers it.
pub(crate) fn descriptor_home(unit: &Path) -> Result<PathBuf, String> {
    if unit.extension().is_some_and(|e| e == "plist") {
        return match std::fs::read(unit) {
            Ok(body) => resolve(launchd_job_home(unit, &body)),
            Err(e) => Err(format!("cannot read {}: {e}", unit.display())),
        };
    }
    let Some(name) = unit.file_name().and_then(|n| n.to_str()) else {
        return Err(format!("{} names no systemd unit", unit.display()));
    };
    match systemctl_show(name) {
        Some(show) => resolve(systemd_unit_home(&show)),
        None => Err(format!("cannot query systemd for {name}")),
    }
}

/// `systemctl --user show` of `unit` with the properties this module reads.
pub(crate) fn systemctl_show(unit: &str) -> Option<String> {
    command_stdout(
        "systemctl",
        &["--user", "show", SYSTEMD_SHOW_PROPERTIES, unit],
    )
}

/// [`systemd_service_home`] wired to `/proc` and the user manager.
pub(crate) fn systemd_unit_home(show: &str) -> ServiceHome {
    systemd_service_home(
        show,
        |pid| std::fs::read(format!("/proc/{pid}/environ")).ok(),
        || command_stdout("systemctl", &["--user", "show-environment"]),
    )
}

/// [`launchd_service_home`] for the plist at `path` (raw bytes `plist`),
/// wired to `launchctl print` / the job's process and `launchctl getenv`.
/// The job label is the plist's file stem, the naming `mur` installs with.
pub(crate) fn launchd_job_home(path: &Path, plist: &[u8]) -> ServiceHome {
    let label = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    launchd_service_home(
        &String::from_utf8_lossy(plist),
        || launchd_job_pid(label),
        process_environ,
        || command_stdout("launchctl", &["getenv", MUR_HOME_ENV]),
    )
}

/// Pid of the running launchd job `label` in the user's GUI domain:
/// `Ok(None)` when it is not loaded or loaded but not running, `Err` when
/// launchd could not tell. Off macOS there is no launchd, so never running.
fn launchd_job_pid(label: &str) -> Result<Option<u32>, String> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("{}/{label}", super::launchd_domain());
        let out = std::process::Command::new("launchctl")
            .args(["print", &target])
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("could not run `launchctl print {target}`: {e}"))?;
        match out.status.code() {
            Some(0) => Ok(launchctl_print_pid(&String::from_utf8_lossy(&out.stdout))),
            Some(super::probe::LAUNCHCTL_NO_SUCH_SERVICE) => Ok(None),
            _ => Err(format!(
                "`launchctl print {target}` could not tell whether the job is running"
            )),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
        Ok(None)
    }
}

/// The job's own `pid = N` line from `launchctl print` output. Only the
/// top-level (single-tab) line counts; nested blocks can carry their own.
/// Pure text parsing, so it stays compiled (and unit-tested) everywhere;
/// only the live-job branch of the pid lookup above calls it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launchctl_print_pid(print: &str) -> Option<u32> {
    print.lines().find_map(|l| {
        let l = l.strip_prefix('\t').filter(|l| !l.starts_with('\t'))?;
        l.strip_prefix("pid = ")?.trim().parse().ok()
    })
}

/// Environment (`KEY=VALUE` strings) of process `pid`, through
/// `KERN_PROCARGS2` on macOS. `None` when unreadable: the kernel hands back
/// an empty list rather than an error for a process it will not show, and a
/// launchd job always has some environment, so empty means "cannot read".
fn process_environ(pid: u32) -> Option<Vec<String>> {
    use sysinfo::{Pid, ProcessRefreshKind, System, UpdateKind};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_process_specifics(
        pid,
        ProcessRefreshKind::new().with_environ(UpdateKind::Always),
    );
    let env = sys.process(pid)?.environ();
    (!env.is_empty()).then(|| env.to_vec())
}

fn known(v: &str) -> ServiceHome {
    ServiceHome::Known((!v.is_empty()).then(|| PathBuf::from(v)))
}

/// Effective `MUR_HOME` of the systemd unit described by `systemctl show
/// --property=MainPID,Environment,EnvironmentFiles,UnsetEnvironment`.
///
/// A running unit is answered from its process environment (`read_environ`
/// gets the main pid) — the only source that reflects every override. A
/// stopped one is answered from systemd's merged `Environment=` (fragment
/// plus drop-ins), then the manager environment (`manager_env`, the
/// `systemctl --user show-environment` text) when the unit sets nothing.
pub(crate) fn systemd_service_home(
    show: &str,
    read_environ: impl FnOnce(u32) -> Option<Vec<u8>>,
    manager_env: impl FnOnce() -> Option<String>,
) -> ServiceHome {
    let pid = show_property(show, "MainPID")
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    if pid > 0 {
        return match read_environ(pid) {
            Some(env) => environ_mur_home(&env),
            None => ServiceHome::Unknown(format!(
                "cannot read the environment of the running murmurd (pid {pid})"
            )),
        };
    }
    if show_property(show, "EnvironmentFiles").is_some_and(|f| !f.is_empty()) {
        return ServiceHome::Unknown(
            "the unit loads an EnvironmentFile=, which can set MUR_HOME but is only \
             read when the unit starts"
                .into(),
        );
    }
    let names_home = |w: &String| w == MUR_HOME_ENV || w.starts_with(&format!("{MUR_HOME_ENV}="));
    if show_property(show, "UnsetEnvironment")
        .is_some_and(|u| split_systemd_words(u).iter().any(names_home))
    {
        return ServiceHome::Unknown("the unit's UnsetEnvironment= names MUR_HOME".into());
    }
    if let Some(v) = systemd_env_mur_home(show) {
        return known(&v);
    }
    match manager_env() {
        Some(env) => manager_env_mur_home(&env),
        None => ServiceHome::Unknown("cannot read the systemd user manager's environment".into()),
    }
}

/// Raw `MUR_HOME` value from `Environment=` lines (unit file or `systemctl
/// show` form; last assignment wins), systemd quoting undone. `None` when no
/// line assigns it; `Some("")` when one assigns it empty.
pub(crate) fn systemd_env_mur_home(text: &str) -> Option<String> {
    let prefix = format!("{MUR_HOME_ENV}=");
    let mut found = None;
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("Environment=") else {
            continue;
        };
        for word in split_systemd_words(rest) {
            if let Some(v) = word.strip_prefix(&prefix) {
                found = Some(v.to_string());
            }
        }
    }
    found
}

/// `MUR_HOME` from a `/proc/<pid>/environ` blob (NUL-separated `KEY=VALUE`).
fn environ_mur_home(environ: &[u8]) -> ServiceHome {
    let prefix = format!("{MUR_HOME_ENV}=");
    let Some(raw) = environ
        .split(|b| *b == 0)
        .find_map(|e| e.strip_prefix(prefix.as_bytes()))
    else {
        return ServiceHome::Known(None);
    };
    match std::str::from_utf8(raw) {
        Ok(v) => known(v),
        Err(_) => ServiceHome::Unknown("the running murmurd's MUR_HOME is not UTF-8".into()),
    }
}

/// `MUR_HOME` from `systemctl --user show-environment` output. systemd
/// prints values with special characters as `$'…'`; those are refused
/// rather than half-decoded.
fn manager_env_mur_home(env: &str) -> ServiceHome {
    let prefix = format!("{MUR_HOME_ENV}=");
    match env.lines().find_map(|l| l.strip_prefix(&prefix)) {
        None => ServiceHome::Known(None),
        Some(v) if v.starts_with("$'") => ServiceHome::Unknown(
            "the systemd user manager's MUR_HOME uses quoting MUR does not decode".into(),
        ),
        Some(v) => known(v),
    }
}

/// Effective `MUR_HOME` of the launchd job whose plist is `plist`.
///
/// A running job (`job_pid`) is answered from its process environment
/// (`read_environ`) — the only source that reflects a plist edited or a
/// `launchctl setenv` issued after the job started, the same rule the
/// systemd path follows. A job that is not running is answered from the
/// plist's `EnvironmentVariables`, else launchd's user-domain environment
/// (`launchctl setenv`, read through `domain_env`), which jobs inherit.
pub(crate) fn launchd_service_home(
    plist: &str,
    job_pid: impl FnOnce() -> Result<Option<u32>, String>,
    read_environ: impl FnOnce(u32) -> Option<Vec<String>>,
    domain_env: impl FnOnce() -> Option<String>,
) -> ServiceHome {
    match job_pid() {
        Err(reason) => return ServiceHome::Unknown(reason),
        Ok(Some(pid)) => {
            return match read_environ(pid) {
                Some(env) => environ_strings_mur_home(&env),
                None => ServiceHome::Unknown(format!(
                    "cannot read the environment of the running service (pid {pid})"
                )),
            };
        }
        Ok(None) => {}
    }
    if !plist.contains("<plist") {
        return ServiceHome::Unknown(
            "the launchd plist is not in XML form, so its MUR_HOME cannot be read".into(),
        );
    }
    if let Some(h) = super::launchd_plist_mur_home(plist) {
        return ServiceHome::Known(Some(h));
    }
    if plist.contains(MUR_HOME_ENV) {
        return ServiceHome::Unknown(
            "the launchd plist sets MUR_HOME in a form MUR cannot read".into(),
        );
    }
    match domain_env() {
        Some(v) => known(v.trim()),
        None => ServiceHome::Unknown("cannot read launchd's user-domain environment".into()),
    }
}

/// `MUR_HOME` from a decoded process environment. The decoding is lossy, so
/// a value carrying U+FFFD was not UTF-8 and is refused, not guessed at.
fn environ_strings_mur_home(env: &[String]) -> ServiceHome {
    let prefix = format!("{MUR_HOME_ENV}=");
    match env.iter().find_map(|e| e.strip_prefix(&prefix)) {
        None => ServiceHome::Known(None),
        Some(v) if v.contains('\u{FFFD}') => {
            ServiceHome::Unknown("the running service's MUR_HOME is not UTF-8".into())
        }
        Some(v) => known(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const STOPPED: &str = "MainPID=0\nEnvironmentFiles=\nUnsetEnvironment=\n";

    fn no_proc(_: u32) -> Option<Vec<u8>> {
        panic!("a stopped unit has no process environment to read")
    }

    #[test]
    fn running_unit_answers_from_its_process_environment() {
        // Nothing in `Environment=` (the drop-in may not even be reloaded),
        // yet the live daemon runs the alternate home: the process wins.
        let show = "MainPID=42\nEnvironment=\n";
        let env = b"HOME=/home/u\0MUR_HOME=/data/alternate\0PATH=/bin\0".to_vec();
        let got = systemd_service_home(show, |pid| (pid == 42).then_some(env), || None);
        assert_eq!(got, ServiceHome::Known(Some("/data/alternate".into())));

        let default = systemd_service_home(show, |_| Some(b"HOME=/h\0".to_vec()), || None);
        assert_eq!(default, ServiceHome::Known(None));

        // Unreadable is not "default home".
        let blind = systemd_service_home(show, |_| None, || Some(String::new()));
        assert!(matches!(blind, ServiceHome::Unknown(_)), "{blind:?}");
    }

    #[test]
    fn dropin_override_of_mur_home_is_the_effective_home() {
        // `systemctl show` merges fragment + drop-ins into one property: the
        // installed default unit sets no MUR_HOME, the drop-in adds it.
        let show = format!("{STOPPED}Environment=PATH=/usr/bin MUR_HOME=/data/alternate\n");
        let got = systemd_service_home(&show, no_proc, || Some("MUR_HOME=/manager\n".into()));
        assert_eq!(got, ServiceHome::Known(Some("/data/alternate".into())));

        // The drop-in's quoted form, as `systemctl show` prints it.
        let show = format!("{STOPPED}Environment=\"MUR_HOME=/data/my mur\"\n");
        let got = systemd_service_home(&show, no_proc, || None);
        assert_eq!(got, ServiceHome::Known(Some("/data/my mur".into())));

        // An empty override still beats the manager environment.
        let show = format!("{STOPPED}Environment=MUR_HOME=\n");
        let got = systemd_service_home(&show, no_proc, || Some("MUR_HOME=/m\n".into()));
        assert_eq!(got, ServiceHome::Known(None));
    }

    #[test]
    fn stopped_unit_without_mur_home_inherits_the_manager_environment() {
        let show = format!("{STOPPED}Environment=PATH=/usr/bin\n");
        let got = systemd_service_home(&show, no_proc, || Some("MUR_HOME=/env/d\n".into()));
        assert_eq!(got, ServiceHome::Known(Some("/env/d".into())));

        let got = systemd_service_home(&show, no_proc, || Some("LANG=C\n".into()));
        assert_eq!(got, ServiceHome::Known(None));

        for unreadable in [None, Some("MUR_HOME=$'/a\\nb'\n".to_string())] {
            let got = systemd_service_home(&show, no_proc, || unreadable);
            assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
        }
    }

    #[test]
    fn overrides_mur_cannot_evaluate_refuse_ownership() {
        let show = "MainPID=0\nEnvironmentFiles=/etc/murmurd.env (ignore_errors=no)\n\
                    Environment=MUR_HOME=/x\n";
        let got = systemd_service_home(show, no_proc, || Some(String::new()));
        assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");

        let show = "MainPID=0\nUnsetEnvironment=MUR_HOME\nEnvironment=MUR_HOME=/x\n";
        let got = systemd_service_home(show, no_proc, || Some(String::new()));
        assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
    }

    fn stopped() -> Result<Option<u32>, String> {
        Ok(None)
    }

    fn no_environ(_: u32) -> Option<Vec<String>> {
        panic!("a job that is not running has no process environment to read")
    }

    #[test]
    fn launchd_home_comes_from_plist_then_domain_environment() {
        let plist = |env: &str| format!("<plist><dict>{env}\n</dict></plist>");
        let set = plist(&super::super::launchd_env_block(Some(Path::new("/data/a"))));
        assert_eq!(
            launchd_service_home(&set, stopped, no_environ, || panic!("the plist answers")),
            ServiceHome::Known(Some("/data/a".into()))
        );
        let unset = plist("<key>Label</key>");
        assert_eq!(
            launchd_service_home(&unset, stopped, no_environ, || Some(
                "/setenv/home\n".into()
            )),
            ServiceHome::Known(Some("/setenv/home".into()))
        );
        assert_eq!(
            launchd_service_home(&unset, stopped, no_environ, || Some("\n".into())),
            ServiceHome::Known(None)
        );
        for (body, env) in [
            (unset.as_str(), None),
            ("bplist00 binary", Some(String::new())),
            (
                "<plist><key>MUR_HOME</key><data>x</data></plist>",
                Some(String::new()),
            ),
        ] {
            let got = launchd_service_home(body, stopped, no_environ, || env);
            assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
        }
    }

    #[test]
    fn running_launchd_job_answers_from_its_process_environment() {
        // The plist names one home, the live job runs another (plist edited
        // after load, or `launchctl setenv` before it started): the process
        // wins, and neither the plist nor the domain is consulted.
        let set = format!(
            "<plist><dict>{}</dict></plist>",
            super::super::launchd_env_block(Some(Path::new("/data/plist")))
        );
        let env = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let live = env(&["PATH=/usr/bin", "MUR_HOME=/data/live", "XPC_SERVICE_NAME=x"]);
        let got = launchd_service_home(
            &set,
            || Ok(Some(42)),
            |pid| (pid == 42).then_some(live),
            || panic!("the process answers"),
        );
        assert_eq!(got, ServiceHome::Known(Some("/data/live".into())));

        // A running job without MUR_HOME is the default home, even though the
        // plist (not reloaded) still sets one. A binary plist no longer
        // matters once the job is live.
        let bare = env(&["PATH=/usr/bin"]);
        let got = launchd_service_home(&set, || Ok(Some(7)), |_| Some(bare), || None);
        assert_eq!(got, ServiceHome::Known(None));
        let bare = env(&["MUR_HOME=/b"]);
        let got = launchd_service_home("bplist00", || Ok(Some(7)), |_| Some(bare), || None);
        assert_eq!(got, ServiceHome::Known(Some("/b".into())));

        // Unreadable, undecodable, or launchd unable to say: never "default".
        let odd = env(&["MUR_HOME=/a\u{FFFD}b"]);
        for got in [
            launchd_service_home(&set, || Ok(Some(7)), |_| None, || Some(String::new())),
            launchd_service_home(&set, || Ok(Some(7)), |_| Some(odd), || None),
            launchd_service_home(&set, || Err("launchd said 5".into()), no_environ, || None),
        ] {
            assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
        }
    }

    #[test]
    fn launchctl_print_pid_reads_only_the_jobs_own_line() {
        let print = "gui/501/run.mur.murmurd = {\n\tstate = running\n\
                     \tendpoints = {\n\t\tpid = 1\n\t}\n\tpid = 86318\n}\n";
        assert_eq!(launchctl_print_pid(print), Some(86318));
        let idle = "gui/501/x = {\n\tstate = not running\n\t\tpid = 9\n}\n";
        assert_eq!(launchctl_print_pid(idle), None);
    }
}
