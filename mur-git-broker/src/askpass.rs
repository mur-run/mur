//! Hands an https token to one `git push` without it touching argv, env, a file, or the URL
//! (https design §5).
//!
//! The parent writes the token into a pipe, keeps the read end `CLOEXEC`, and in the push child
//! only (`pre_exec`) `dup2`s it to [`ASKPASS_FD`]. git runs `GIT_ASKPASS` — this crate's host
//! binary, recognised by `MUR_ASKPASS_FD` being set — which answers a `Password` prompt from that
//! fd. The username goes in through `credential.username`, so git asks for the password only.
//!
//! Everything here fails closed: a missing fd, an empty read, or a prompt that is not exactly the
//! expected origin produces no answer, and git reports an authentication failure.

use crate::push::RemoteAuth;
use secrecy::{ExposeSecret, SecretString};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::Command,
};

/// Set (to [`ASKPASS_FD`]) only for an askpass invocation; its presence is how the host binary
/// knows to behave as askpass instead of as itself.
pub const ENV_FD: &str = "MUR_ASKPASS_FD";
/// The only prompt origin (`scheme://user@host[:port]`) the askpass will answer.
pub const ENV_ORIGIN: &str = "MUR_ASKPASS_ORIGIN";
/// The descriptor number the token has in the push child and everything it spawns.
pub const ASKPASS_FD: i32 = 57;
/// A token longer than this is refused up front: it must fit a pipe buffer so that writing it
/// before the child exists cannot block.
pub const MAX_TOKEN_BYTES: usize = 4096;

/// Credentials for one https remote. Built per request, dropped (and zeroized) when `push`
/// returns.
pub struct TokenAuth {
    secret: SecretString,
    username: String,
    /// `scheme://user@host[:port]`, exactly as git prints it in a `Password` prompt.
    origin: String,
    askpass_bin: PathBuf,
}

impl TokenAuth {
    /// `remote_url` is the bound `https://host[:port]/path` URL; only its origin is kept.
    pub fn new(
        secret: SecretString,
        username: &str,
        remote_url: &str,
        askpass_bin: PathBuf,
    ) -> Option<Self> {
        Self::build("https://", secret, username, remote_url, askpass_bin)
    }

    /// Like [`Self::new`] but for `http://`, so a test can run real git against a local plain
    /// server (the credential flow does not depend on TLS). Never compiled into a release build.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_plain_http_for_test(
        secret: SecretString,
        username: &str,
        remote_url: &str,
        askpass_bin: PathBuf,
    ) -> Option<Self> {
        Self::build("http://", secret, username, remote_url, askpass_bin)
    }

    fn build(
        scheme: &str,
        secret: SecretString,
        username: &str,
        remote_url: &str,
        askpass_bin: PathBuf,
    ) -> Option<Self> {
        let rest = remote_url.strip_prefix(scheme)?;
        let authority = rest.split('/').next().filter(|a| !a.is_empty())?;
        if authority.contains('@') || secret.expose_secret().len() > MAX_TOKEN_BYTES {
            return None;
        }
        Some(Self {
            secret,
            origin: format!("{scheme}{username}@{authority}"),
            username: username.to_owned(),
            askpass_bin,
        })
    }

    /// A fresh pipe holding the token, read end only. One per command: the fd is read once.
    fn token_pipe(&self) -> Option<OwnedFd> {
        let (reader, mut writer) = std::io::pipe().ok()?;
        writer
            .write_all(self.secret.expose_secret().as_bytes())
            .ok()?;
        drop(writer);
        Some(OwnedFd::from(reader))
    }
}

impl RemoteAuth for TokenAuth {
    /// Adds the askpass wiring. If the pipe cannot be made nothing is added, so git (which has
    /// `GIT_TERMINAL_PROMPT=0`) fails authentication rather than being offered a wrong value.
    fn apply(&self, cmd: &mut Command) {
        let Some(fd) = self.token_pipe() else { return };
        cmd.env("GIT_ASKPASS", &self.askpass_bin)
            .env(ENV_FD, ASKPASS_FD.to_string())
            .env(ENV_ORIGIN, &self.origin)
            // `-c` cannot be used: the argv is already built and `apply` only appends. Env config
            // also keeps the username out of argv.
            .env("GIT_CONFIG_COUNT", "3")
            .env("GIT_CONFIG_KEY_0", "credential.username")
            .env("GIT_CONFIG_VALUE_0", &self.username)
            .env("GIT_CONFIG_KEY_1", "credential.helper")
            .env("GIT_CONFIG_VALUE_1", "")
            .env("GIT_CONFIG_KEY_2", "http.followRedirects")
            .env("GIT_CONFIG_VALUE_2", "false");
        // SAFETY: the closure runs between fork and exec, so it may only call async-signal-safe
        // functions. It calls `fcntl` and `dup2` on integers and builds an `io::Error` from an
        // errno (no allocation); it takes no lock and allocates nothing. The `OwnedFd` is moved
        // into the closure only to keep it open until the `Command` is dropped; it is never closed
        // here. The parent's copy stays `CLOEXEC`, so only the child sees `ASKPASS_FD`.
        unsafe {
            cmd.pre_exec(move || {
                let src = fd.as_raw_fd();
                let rc = if src == ASKPASS_FD {
                    // `dup2` onto itself is a no-op and would leave `CLOEXEC` set.
                    libc::fcntl(src, libc::F_SETFD, 0)
                } else {
                    libc::dup2(src, ASKPASS_FD)
                };
                if rc < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// The answer for one askpass call, or `None` for "do not answer". `prompt` is git's argv[1].
pub fn answer(prompt: &str, origin: &str, token: &mut impl Read) -> Option<Vec<u8>> {
    let quoted = prompt.trim_end().strip_prefix("Password for '")?;
    if quoted.strip_suffix("':")? != origin {
        return None;
    }
    let mut buf = Vec::new();
    token
        .take(MAX_TOKEN_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    // Empty: already consumed. Oversized: not something we wrote.
    if buf.is_empty() || buf.len() > MAX_TOKEN_BYTES {
        return None;
    }
    Some(buf)
}

/// Entry point for the host binary when `MUR_ASKPASS_FD` is set. Returns the exit code.
pub fn run_from_env() -> i32 {
    let fd: i32 = match std::env::var(ENV_FD).ok().and_then(|v| v.parse().ok()) {
        Some(n) if n > 2 => n,
        _ => return 1,
    };
    let (Some(prompt), Ok(origin)) = (std::env::args().nth(1), std::env::var(ENV_ORIGIN)) else {
        return 1;
    };
    // SAFETY: `fd` was placed here by `TokenAuth::apply` for this process tree and nothing else
    // in this short-lived process owns it.
    let mut token = unsafe { std::fs::File::from_raw_fd(fd) };
    match answer(&prompt, &origin, &mut token) {
        Some(mut t) => {
            t.push(b'\n');
            if std::io::stdout().write_all(&t).is_err() {
                return 1;
            }
            0
        }
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const ORIGIN: &str = "https://u@git.example.com";
    fn ask(prompt: &str, token: &[u8]) -> Option<Vec<u8>> {
        answer(prompt, ORIGIN, &mut Cursor::new(token.to_vec()))
    }

    #[test]
    fn answers_the_exact_origin() {
        let p = "Password for 'https://u@git.example.com': ";
        assert_eq!(ask(p, b"tok").unwrap(), b"tok");
    }

    #[test]
    fn refuses_another_host_user_scheme_or_port() {
        for p in [
            "Password for 'https://u@evil.example.com': ",
            "Password for 'https://other@git.example.com': ",
            "Password for 'https://git.example.com': ",
            "Password for 'http://u@git.example.com': ",
            "Password for 'https://u@git.example.com:8443': ",
            "Password for 'https://u@git.example.com.evil.net': ",
        ] {
            assert!(ask(p, b"tok").is_none(), "{p}");
        }
    }

    #[test]
    fn refuses_a_username_prompt_and_garbage() {
        assert!(ask("Username for 'https://git.example.com': ", b"tok").is_none());
        assert!(ask("", b"tok").is_none());
    }

    #[test]
    fn an_empty_fd_gets_no_answer() {
        assert!(ask("Password for 'https://u@git.example.com': ", b"").is_none());
    }

    #[test]
    fn new_requires_https_and_no_userinfo_and_a_bounded_token() {
        let s = |t: &str| SecretString::from(t.to_owned());
        let bin = || PathBuf::from("/bin/askpass");
        assert!(TokenAuth::new(s("t"), "u", "https://h.example/p.git", bin()).is_some());
        assert!(TokenAuth::new(s("t"), "u", "http://h.example/p.git", bin()).is_none());
        assert!(TokenAuth::new(s("t"), "u", "https://x@h.example/p.git", bin()).is_none());
        assert!(TokenAuth::new(s(&"a".repeat(5000)), "u", "https://h/p", bin()).is_none());
    }

    #[test]
    fn the_child_reads_the_token_on_the_fixed_fd_and_the_parent_does_not_leak_it() {
        let auth = TokenAuth::new(
            SecretString::from("tok-123".to_owned()),
            "u",
            "https://h.example/p.git",
            PathBuf::from("/bin/askpass"),
        )
        .unwrap();
        // bash, not sh: dash (Ubuntu) rejects multi-digit fds such as `<&57`.
        let mut cmd = Command::new("/bin/bash");
        cmd.args([
            "-c",
            "cat <&57; echo; echo \"$MUR_ASKPASS_ORIGIN\"; echo \"$GIT_CONFIG_VALUE_0\"",
        ]);
        auth.apply(&mut cmd);
        let out = cmd.output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text, "tok-123\nhttps://u@h.example\nu\n");
        // A second command gets its own pipe: the fd is not shared between children.
        let mut again = Command::new("/bin/bash");
        again.args(["-c", "cat <&57"]);
        auth.apply(&mut again);
        assert_eq!(again.output().unwrap().stdout, b"tok-123");
    }
}
