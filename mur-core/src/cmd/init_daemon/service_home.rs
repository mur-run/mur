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

use std::path::PathBuf;

use mur_common::home::MUR_HOME_ENV;

use super::{show_property, split_systemd_words};

/// The `MUR_HOME` a murmurd service runs with, as far as it can be proven.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServiceHome {
    /// The service's `MUR_HOME` (`None`: unset or empty, i.e. the default home).
    Known(Option<PathBuf>),
    /// Ownership cannot be established; the reason is shown to the user.
    Unknown(String),
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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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

/// Effective `MUR_HOME` of the launchd job whose plist is `plist`: the
/// plist's `EnvironmentVariables`, else launchd's user-domain environment
/// (`launchctl setenv`, read through `domain_env`), which jobs inherit.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn launchd_service_home(
    plist: &str,
    domain_env: impl FnOnce() -> Option<String>,
) -> ServiceHome {
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

    #[test]
    fn launchd_home_comes_from_plist_then_domain_environment() {
        let plist = |env: &str| format!("<plist><dict>{env}\n</dict></plist>");
        let set = plist(&super::super::launchd_env_block(Some(Path::new("/data/a"))));
        assert_eq!(
            launchd_service_home(&set, || panic!("the plist already answers")),
            ServiceHome::Known(Some("/data/a".into()))
        );
        let unset = plist("<key>Label</key>");
        assert_eq!(
            launchd_service_home(&unset, || Some("/setenv/home\n".into())),
            ServiceHome::Known(Some("/setenv/home".into()))
        );
        assert_eq!(
            launchd_service_home(&unset, || Some("\n".into())),
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
            let got = launchd_service_home(body, || env);
            assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
        }
    }
}
