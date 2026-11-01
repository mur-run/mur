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

/// A service's `MUR_HOME` per lifecycle step. Stopping acts on the instance
/// running now; starting or restarting brings up whatever the service
/// manager would launch next. The two can differ, so each step gets its own
/// answer instead of one verdict that is wrong for one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceHomes {
    /// Home of the running instance; `None` when nothing is running.
    pub(crate) running: Option<ServiceHome>,
    /// Home the service manager's next start would get.
    pub(crate) next_start: ServiceHome,
}

impl ServiceHomes {
    fn both(reason: String) -> Self {
        Self {
            running: Some(ServiceHome::Unknown(reason.clone())),
            next_start: ServiceHome::Unknown(reason),
        }
    }

    /// Home a stop acts on: the running instance, else (nothing running)
    /// the one a start would bring up.
    pub(crate) fn for_stop(self) -> ServiceHome {
        self.running.unwrap_or(self.next_start)
    }

    /// Home a start or restart authorizes: a running instance and the next
    /// start must agree, since a restart replaces the one with the other.
    pub(crate) fn for_start(self) -> ServiceHome {
        match self.running {
            None => self.next_start,
            Some(live) => agree(live, self.next_start),
        }
    }
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

/// The data root the service whose descriptor is `unit` (a launchd plist or
/// a systemd user unit file) serves after its next (re)start, as [`resolve`]
/// answers it.
pub(crate) fn descriptor_home(unit: &Path) -> Result<PathBuf, String> {
    if unit.extension().is_some_and(|e| e == "plist") {
        return match std::fs::read(unit) {
            Ok(body) => resolve(launchd_job_home(unit, &body).for_start()),
            Err(e) => Err(format!("cannot read {}: {e}", unit.display())),
        };
    }
    let Some(name) = unit.file_name().and_then(|n| n.to_str()) else {
        return Err(format!("{} names no systemd unit", unit.display()));
    };
    match systemctl_show(name) {
        Some(show) => resolve(systemd_unit_home(&show).for_start()),
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
pub(crate) fn systemd_unit_home(show: &str) -> ServiceHomes {
    systemd_service_home(
        show,
        |pid| std::fs::read(format!("/proc/{pid}/environ")).ok(),
        || command_stdout("systemctl", &["--user", "show-environment"]),
    )
}

/// [`launchd_service_home`] for the plist at `path` (raw bytes `plist`),
/// wired to `launchctl print` / the job's process and `launchctl getenv`.
/// The job label is the plist's file stem, the naming `mur` installs with.
pub(crate) fn launchd_job_home(path: &Path, plist: &[u8]) -> ServiceHomes {
    let label = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    launchd_service_home(
        &String::from_utf8_lossy(plist),
        launchd_job_print(label),
        process_environ,
        || command_stdout("launchctl", &["getenv", MUR_HOME_ENV]),
    )
}

/// `launchctl print` of the loaded launchd job `label` in the user's GUI
/// domain: `Ok(None)` when launchd has no such job loaded, `Err` when it
/// could not tell. Off macOS there is no launchd, so never loaded.
fn launchd_job_print(label: &str) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        let target = format!("{}/{label}", super::launchd_domain());
        let out = std::process::Command::new("launchctl")
            .args(["print", &target])
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("could not run `launchctl print {target}`: {e}"))?;
        match out.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
            Some(super::probe::LAUNCHCTL_NO_SUCH_SERVICE) => Ok(None),
            _ => Err(format!(
                "`launchctl print {target}` could not tell whether the job is loaded"
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
/// --property=MainPID,Environment,EnvironmentFiles,UnsetEnvironment`, per
/// lifecycle step (see [`ServiceHomes`]).
///
/// The next start reads the loaded unit's merged `Environment=` (fragment
/// plus reloaded drop-ins), else the manager environment (`manager_env`,
/// the `systemctl --user show-environment` text). A running unit is read
/// from its process environment (`read_environ` gets the main pid): a
/// drop-in reloaded but not yet applied, or a changed manager environment,
/// makes the two differ.
pub(crate) fn systemd_service_home(
    show: &str,
    read_environ: impl FnOnce(u32) -> Option<Vec<u8>>,
    manager_env: impl FnOnce() -> Option<String>,
) -> ServiceHomes {
    let pid = show_property(show, "MainPID")
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    let running = (pid != 0).then(|| match read_environ(pid) {
        Some(env) => environ_mur_home(&env),
        None => ServiceHome::Unknown(format!(
            "cannot read the environment of the running murmurd (pid {pid})"
        )),
    });
    ServiceHomes {
        running,
        next_start: systemd_next_start_home(show, manager_env),
    }
}

/// `MUR_HOME` the next start of the unit in `show` would receive.
fn systemd_next_start_home(
    show: &str,
    manager_env: impl FnOnce() -> Option<String>,
) -> ServiceHome {
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

/// Effective `MUR_HOME` of the launchd job whose plist is `plist`, per
/// lifecycle step (see [`ServiceHomes`]).
///
/// `print` is `launchctl print` of the job (`Ok(None)`: not loaded). A
/// loaded job is restarted (`kickstart -k`) from its LOADED definition:
/// launchd reads the plist only at `bootstrap`, so an edited plist changes
/// nothing until the job is booted out and bootstrapped again. Its next
/// start therefore reads the `environment` block `print` shows, and only an
/// unloaded job (whose start falls back to `bootstrap`) reads the plist on
/// disk. Either way a job that does not set `MUR_HOME` inherits launchd's
/// user-domain environment (`launchctl setenv`, read through `domain_env`).
/// A running job is read from its process environment (`read_environ`).
pub(crate) fn launchd_service_home(
    plist: &str,
    print: Result<Option<String>, String>,
    read_environ: impl FnOnce(u32) -> Option<Vec<String>>,
    domain_env: impl FnOnce() -> Option<String>,
) -> ServiceHomes {
    let print = match print {
        Err(reason) => return ServiceHomes::both(reason),
        Ok(print) => print,
    };
    let Some(print) = print else {
        return ServiceHomes {
            running: None,
            next_start: launchd_bootstrap_home(plist, domain_env),
        };
    };
    let running = launchctl_print_pid(&print).map(|pid| match read_environ(pid) {
        Some(env) => environ_strings_mur_home(&env),
        None => ServiceHome::Unknown(format!(
            "cannot read the environment of the running service (pid {pid})"
        )),
    });
    let next_start = match launchctl_print_environment(&print) {
        None => ServiceHome::Unknown(
            "`launchctl print` shows no environment for the loaded job, so what its \
             next start gets cannot be read"
                .into(),
        ),
        Some(env) => match env.into_iter().find(|(k, _)| k == MUR_HOME_ENV) {
            Some((_, v)) => known(&v),
            None => launchd_domain_home(domain_env),
        },
    };
    ServiceHomes {
        running,
        next_start,
    }
}

/// `MUR_HOME` a `bootstrap` of the plist `plist` would give the job.
fn launchd_bootstrap_home(plist: &str, domain_env: impl FnOnce() -> Option<String>) -> ServiceHome {
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
    launchd_domain_home(domain_env)
}

/// `MUR_HOME` from launchd's user-domain environment.
fn launchd_domain_home(domain_env: impl FnOnce() -> Option<String>) -> ServiceHome {
    match domain_env() {
        Some(v) => known(launchctl_getenv_value(&v)),
        None => ServiceHome::Unknown("cannot read launchd's user-domain environment".into()),
    }
}

/// The loaded job's own `environment = { KEY => VALUE … }` block from
/// `launchctl print` (the plist's `EnvironmentVariables` plus what launchd
/// adds, e.g. `XPC_SERVICE_NAME`). `None` when there is no such block.
/// `inherited environment` / `default environment` are other blocks.
fn launchctl_print_environment(print: &str) -> Option<Vec<(String, String)>> {
    let mut lines = print.lines();
    lines.find(|l| *l == "\tenvironment = {")?;
    let mut env = Vec::new();
    for l in lines {
        if l == "\t}" {
            return Some(env);
        }
        let entry = l.strip_prefix("\t\t")?;
        let (k, v) = entry.split_once(" => ")?;
        env.push((k.to_string(), v.to_string()));
    }
    None
}

/// The value `launchctl getenv` printed. It appends exactly one newline;
/// only that is removed, because spaces at either end of a path are part
/// of it (`"/data/mur "` and `"/data/mur"` are different directories).
fn launchctl_getenv_value(out: &str) -> &str {
    out.strip_suffix('\n').unwrap_or(out)
}

/// One home for the running instance and the next start, or `Unknown`
/// saying why not. Values are compared as set, not resolved: an explicit
/// default path versus an unset one is refused rather than reasoned about.
fn agree(live: ServiceHome, next: ServiceHome) -> ServiceHome {
    let shown = |h: &Option<PathBuf>| match h {
        Some(p) => format!("MUR_HOME={}", p.display()),
        None => "no MUR_HOME".to_string(),
    };
    match (live, next) {
        (ServiceHome::Unknown(r), _) => ServiceHome::Unknown(r),
        (ServiceHome::Known(_), ServiceHome::Unknown(r)) => ServiceHome::Unknown(format!(
            "what the service would run with after a restart cannot be told: {r}"
        )),
        (ServiceHome::Known(a), ServiceHome::Known(b)) if a == b => ServiceHome::Known(a),
        (ServiceHome::Known(a), ServiceHome::Known(b)) => ServiceHome::Unknown(format!(
            "the running service has {}, but its next start would get {}; restart it \
             with the service manager once the intended home is settled",
            shown(&a),
            shown(&b)
        )),
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
#[path = "service_home_tests.rs"]
mod tests;
