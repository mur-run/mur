//! The bash child's base environment — an allowlist, not a filter.
//!
//! `bash_jobs` spawns with `env_clear()`, so a key reaches the child only if
//! it is named here (or added later by the track, vault, or scratch layers).
//! Anything the runtime itself inherited — launchd's `SSH_AUTH_SOCK`, a token
//! exported in a shell profile — stays out unless deliberately listed.

/// Inherited keys the child may see. The minimum promised to user tools, not
/// a list of what happens to be set: adding a key here widens what every
/// agent's shell can read, so each needs a tool that breaks without it.
pub const INHERITED_ENV_KEYS: &[&str] = &[
    // Identity + shell basics most tools assume.
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    // Locale — without these, Python/Perl warn and some tools mis-decode UTF-8.
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    // macOS CoreFoundation text encoding; `security`, `defaults` read it.
    "__CF_USER_TEXT_ENCODING",
    // Scratch defaults; the scratch layer overrides these when it has a dir.
    "TMPDIR",
    "TMP",
    "TEMP",
    // Issue #015: tool caches that otherwise fall back to sandbox-denied paths.
    "NPM_CONFIG_CACHE",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "PYTHONUSERBASE",
    // TLS trust overrides; dropping them breaks HTTPS silently behind a custom CA.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    // A `mur` command run from the shell must address the same home.
    mur_common::home::MUR_HOME_ENV,
];

/// `PATH` (augmented with the exec dirs) plus every allowlisted key the
/// runtime has. Later layers append to this; `Command::envs` lets the last
/// entry for a key win.
pub fn base_env(augmented_path: String) -> Vec<(String, String)> {
    base_env_from(augmented_path, |k| std::env::var(k).ok())
}

fn base_env_from(
    augmented_path: String,
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut env = vec![("PATH".to_string(), augmented_path)];
    env.extend(
        INHERITED_ENV_KEYS
            .iter()
            .filter_map(|k| lookup(k).map(|v| (k.to_string(), v))),
    );
    env
}

/// The key the ssh client reads its agent socket from.
pub const SSH_AUTH_SOCK_ENV: &str = "SSH_AUTH_SOCK";

/// What a bash child gets for ssh while the ssh-agent proxy runs.
#[derive(Debug, Clone)]
pub struct SshChild {
    /// The proxy socket, exported as `SSH_AUTH_SOCK`.
    pub sock: std::path::PathBuf,
    /// Pinned client settings (plan O2). `None` when no real ssh was found in
    /// the seal's exec dirs: the socket is still exported.
    pub client: Option<super::ssh_client_env::SshClientEnv>,
}

/// ssh keys for a bash child: `SSH_AUTH_SOCK` from the proxy path (never
/// inherited, so the runtime's own upstream value cannot reach the child),
/// plus `GIT_SSH_COMMAND` and `BASH_FUNC_ssh%%` when `client` is set. Nothing
/// when the proxy is not running.
pub fn ssh_child_env(ssh: Option<&SshChild>) -> Vec<(String, String)> {
    let Some(ssh) = ssh else { return Vec::new() };
    let mut env = vec![(
        SSH_AUTH_SOCK_ENV.to_string(),
        ssh.sock.to_string_lossy().into_owned(),
    )];
    if let Some(c) = &ssh.client {
        env.extend(c.env_pairs());
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allowlisted_keys_pass_through() {
        let env = base_env_from("/p".into(), |k| match k {
            "HOME" | "SSH_AUTH_SOCK" | "FAKE_API_TOKEN" => Some("x".into()),
            _ => None,
        });
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["PATH", "HOME"]);
    }

    #[test]
    fn ssh_env_follows_proxy_and_client() {
        let sock = std::path::PathBuf::from("/h/agents/a/ssh-agent.sock");
        let keys = |s: Option<&SshChild>| -> Vec<String> {
            ssh_child_env(s).into_iter().map(|(k, _)| k).collect()
        };
        assert!(keys(None).is_empty());
        let bare = SshChild {
            sock: sock.clone(),
            client: None,
        };
        assert_eq!(
            ssh_child_env(Some(&bare)),
            [("SSH_AUTH_SOCK".into(), "/h/agents/a/ssh-agent.sock".into())]
        );
        let client = super::super::ssh_client_env::SshClientEnv::for_test("/h/agents/a");
        let full = SshChild {
            sock,
            client: Some(client),
        };
        assert_eq!(
            keys(Some(&full)),
            ["SSH_AUTH_SOCK", "GIT_SSH_COMMAND", "BASH_FUNC_ssh%%"]
        );
    }

    #[test]
    fn agent_socket_and_secrets_are_never_allowlisted() {
        for k in ["SSH_AUTH_SOCK", "MUR_RUNTIME_EXPECTED_NAME", "GITHUB_TOKEN"] {
            assert!(
                !INHERITED_ENV_KEYS.contains(&k),
                "{k} must not be inherited"
            );
        }
    }
}
