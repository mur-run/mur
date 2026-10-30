//! Pinned ssh client settings for the bash child (plan O2), passed as env.
//!
//! When the ssh-agent proxy runs, bash children get two extra keys:
//! - `BASH_FUNC_ssh%%` — an exported bash function, so bare `ssh` in the
//!   child's shell runs the real ssh with the flags below first;
//! - `GIT_SSH_COMMAND` — the same command line, for git.
//!
//! There is no wrapper file: a sealed agent may exec only binaries under
//! `spawn_search_dirs` unless spawn mode is `Any`, so a file in the agent home
//! would run only on `--full-perms` boots.
//!
//! These settings keep the honest path predictable (pinned `known_hosts`,
//! strict host key checking, only proxy-listed keys). They are not where
//! authorization is decided — the agent can override them, and the proxy's
//! verified session-bind is what gates a sign request.

use std::path::{Path, PathBuf};

/// git runs this through `sh -c` in place of `ssh`.
pub const GIT_SSH_COMMAND_ENV: &str = "GIT_SSH_COMMAND";
/// bash (≥ 4.3 and Apple's patched 3.2) imports `BASH_FUNC_<name>%%` as a
/// function named `<name>`.
pub const BASH_FUNC_SSH_ENV: &str = "BASH_FUNC_ssh%%";
/// The client binary name searched for in the exec dirs.
pub const SSH_BIN: &str = "ssh";

/// The real ssh and the agent's pinned `known_hosts`, resolved once at boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshClientEnv {
    real_ssh: PathBuf,
    known_hosts: PathBuf,
}

impl SshClientEnv {
    /// `None` when no executable `ssh` is in `exec_dirs`, or when a path
    /// cannot be expressed in an ssh `-o` value (see [`ssh_quote`]).
    pub fn resolve(agent_home: &Path, exec_dirs: &[PathBuf]) -> Option<Self> {
        let real_ssh = resolve_real_ssh(exec_dirs)?;
        Self::with_paths(real_ssh, mur_common::agent::known_hosts_path(agent_home))
    }

    /// `/usr/bin/ssh` with `<agent_home>/ssh/known_hosts`, for tests that
    /// need a value but not a real binary.
    #[cfg(test)]
    pub(crate) fn for_test(agent_home: &str) -> Self {
        Self::with_paths(
            PathBuf::from("/usr/bin/ssh"),
            mur_common::agent::known_hosts_path(Path::new(agent_home)),
        )
        .unwrap()
    }

    fn with_paths(real_ssh: PathBuf, known_hosts: PathBuf) -> Option<Self> {
        ssh_quote(&known_hosts)?;
        Some(Self {
            real_ssh,
            known_hosts,
        })
    }

    /// Arguments placed before the caller's. Order is fixed: for `-o`,
    /// OpenSSH keeps the first value it sees and command-line `-o` beats any
    /// `-F` file, so a later caller `-o` or `-F` cannot change these.
    pub fn pinned_args(&self) -> Vec<String> {
        let kh = ssh_quote(&self.known_hosts).expect("checked in with_paths");
        vec![
            "-o".into(),
            format!("UserKnownHostsFile={kh}"),
            "-o".into(),
            "GlobalKnownHostsFile=none".into(),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            "UpdateHostKeys=no".into(),
            "-o".into(),
            "IdentityFile=none".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-F".into(),
            "none".into(),
        ]
    }

    /// Real ssh plus [`Self::pinned_args`], each word shell-quoted.
    fn command_line(&self) -> String {
        std::iter::once(self.real_ssh.to_string_lossy().into_owned())
            .chain(self.pinned_args())
            .map(|w| shell_escape::unix::escape(w.into()).into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The two env keys for the bash child. The function does not `exec`:
    /// inside a function that would replace the caller's whole shell.
    pub fn env_pairs(&self) -> [(String, String); 2] {
        let cmd = self.command_line();
        [
            (GIT_SSH_COMMAND_ENV.to_string(), cmd.clone()),
            (
                BASH_FUNC_SSH_ENV.to_string(),
                format!("() {{ {cmd} \"$@\"; }}"),
            ),
        ]
    }
}

/// First `dir/ssh` that is an executable regular file. Searched in the seal's
/// exec dirs, not the inherited `PATH`, so the pick is one the seal permits.
pub fn resolve_real_ssh(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(SSH_BIN))
        .find(|p| is_executable_file(p))
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

/// A path as one ssh `-o` value. ssh splits `-o` values itself and lists of
/// files are space-separated, so the path is wrapped in `"`. Measured: a bare
/// `'` gives `invalid quotes`; inside `"` a space or `'` is kept. A `"` in the
/// path is rejected however it is quoted, so that path yields `None`.
fn ssh_quote(p: &Path) -> Option<String> {
    let s = p.to_string_lossy();
    (!s.contains('"')).then(|| format!("\"{s}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: &str) -> SshClientEnv {
        SshClientEnv::with_paths(
            PathBuf::from("/usr/bin/ssh"),
            mur_common::agent::known_hosts_path(Path::new(home)),
        )
        .unwrap()
    }

    /// Expected `-o` value, built with the platform separator so the test
    /// holds on Windows too (`Path::join` gives `\` there).
    fn ukhf(home: &str) -> String {
        let kh = mur_common::agent::known_hosts_path(Path::new(home));
        format!("UserKnownHostsFile=\"{}\"", kh.display())
    }

    #[test]
    fn pinned_args_order_is_fixed() {
        assert_eq!(
            env("/a").pinned_args(),
            [
                "-o",
                ukhf("/a").as_str(),
                "-o",
                "GlobalKnownHostsFile=none",
                "-o",
                "StrictHostKeyChecking=yes",
                "-o",
                "UpdateHostKeys=no",
                "-o",
                "IdentityFile=none",
                "-o",
                "BatchMode=yes",
                "-F",
                "none",
            ]
        );
    }

    #[test]
    fn no_identities_only() {
        assert!(
            !env("/a")
                .pinned_args()
                .iter()
                .any(|a| a.contains("IdentitiesOnly"))
        );
    }

    #[test]
    fn env_pairs_names_and_function_shape() {
        let [(git_k, git_v), (fn_k, fn_v)] = env("/a").env_pairs();
        assert_eq!(git_k, "GIT_SSH_COMMAND");
        assert_eq!(fn_k, "BASH_FUNC_ssh%%");
        assert!(fn_v.starts_with("() { "), "{fn_v}");
        assert!(fn_v.ends_with(" \"$@\"; }"), "{fn_v}");
        assert!(!fn_v.contains("exec"), "{fn_v}");
        assert!(fn_v.contains(&git_v));
    }

    #[test]
    fn awkward_home_round_trips_through_shell_words() {
        let e = env("/x y/it's");
        let [(_, git_v), _] = e.env_pairs();
        let argv = shell_words::split(&git_v).unwrap();
        let mut want = vec!["/usr/bin/ssh".to_string()];
        want.extend(e.pinned_args());
        assert_eq!(argv, want);
        assert_eq!(argv[2], ukhf("/x y/it's"));
    }

    #[test]
    fn double_quote_in_home_yields_none() {
        assert!(
            SshClientEnv::with_paths(PathBuf::from("/usr/bin/ssh"), PathBuf::from("/a\"b/kh"))
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_skips_non_executable_and_picks_executable() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        for (d, mode) in [(&a, 0o644), (&b, 0o755)] {
            std::fs::create_dir(d).unwrap();
            let f = d.join(SSH_BIN);
            std::fs::write(&f, "").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let missing = t.path().join("missing");
        assert_eq!(
            resolve_real_ssh(&[missing, a, b.clone()]),
            Some(b.join(SSH_BIN))
        );
    }

    #[test]
    fn resolve_none_when_absent() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(resolve_real_ssh(&[t.path().to_path_buf()]), None);
        assert_eq!(
            SshClientEnv::resolve(t.path(), &[t.path().to_path_buf()]),
            None
        );
    }
}
