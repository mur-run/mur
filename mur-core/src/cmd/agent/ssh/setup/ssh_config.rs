//! What the user's own `ssh` would use for one host (plan K7): its agent and
//! its identity files, read from `ssh -G` instead of guessed.
//!
//! Many users point `ssh` at another agent with `IdentityAgent` (1Password,
//! Secretive, gpg-agent) and keep no `~/.ssh/id_*` file. `ssh -G` resolves
//! `~/.ssh/config` for the host exactly as a real connection would, and lists
//! the defaults this `ssh` build knows (OpenSSH 10 added `id_mldsa44_ed25519`)
//! when the config names none.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use mur_ssh_request::{RequestHost, SSH_BINARY};

/// `ssh -G` keys this module reads (always printed lowercase).
const KEY_AGENT: &str = "identityagent";
const KEY_FILE: &str = "identityfile";
/// `IdentityAgent` / `IdentityFile` value that turns the option off.
const NONE: &str = "none";
/// `IdentityAgent` value meaning "the inherited environment variable".
const AUTH_SOCK_VAR: &str = "SSH_AUTH_SOCK";

/// Which ssh-agent `ssh` talks to for this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSocket {
    /// The inherited `SSH_AUTH_SOCK`.
    Inherited,
    /// `IdentityAgent <path>` (or `$VAR` that resolved to one).
    Path(PathBuf),
    /// `IdentityAgent none`, or a `$VAR` that is unset: no agent.
    Disabled,
}

/// The parts of `ssh -G <host>` the install connection needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub agent: AgentSocket,
    /// `IdentityFile` values in `ssh`'s own order, unexpanded, `none` dropped.
    pub files: Vec<String>,
}

/// Values the `IdentityFile` tokens expand to. `ssh -G` prints them raw.
#[derive(Debug, Clone)]
pub struct Expand<'a> {
    pub home: &'a Path,
    pub local_user: Option<&'a str>,
    pub host: &'a str,
    pub remote_user: &'a str,
    pub port: u16,
}

/// Parse `ssh -G` output. `env` looks up a `$VAR` agent value.
pub fn parse(output: &str, env: impl Fn(&str) -> Option<String>) -> Resolved {
    let mut agent = AgentSocket::Inherited;
    let mut files = Vec::new();
    for line in output.lines() {
        let Some((key, value)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        match key {
            KEY_AGENT => agent = agent_of(value, &env),
            KEY_FILE if value != NONE && !value.is_empty() => files.push(value.to_string()),
            _ => {}
        }
    }
    Resolved { agent, files }
}

fn agent_of(value: &str, env: &impl Fn(&str) -> Option<String>) -> AgentSocket {
    if value == NONE {
        return AgentSocket::Disabled;
    }
    if value == AUTH_SOCK_VAR {
        return AgentSocket::Inherited;
    }
    if let Some(var) = value.strip_prefix('$') {
        let var = var.trim_start_matches('{').trim_end_matches('}');
        return match env(var).filter(|v| !v.is_empty()) {
            Some(p) => AgentSocket::Path(PathBuf::from(p)),
            None => AgentSocket::Disabled,
        };
    }
    AgentSocket::Path(PathBuf::from(value))
}

/// One `IdentityFile` value as a path, or `None` when it carries a token this
/// cannot expand. Such a file is still offered: `ssh` reads it from the
/// config itself, after the `-i` files.
pub fn expand(raw: &str, x: &Expand) -> Option<PathBuf> {
    let tilde = match raw.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", x.home.display()),
        None if raw == "~" => x.home.display().to_string(),
        None if raw.starts_with('~') => return None, // `~otheruser/`
        None => raw.to_string(),
    };
    let mut out = String::with_capacity(tilde.len());
    let mut chars = tilde.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '%' => out.push('%'),
            'd' => out.push_str(&x.home.display().to_string()),
            'u' => out.push_str(x.local_user?),
            'h' => out.push_str(x.host),
            'r' => out.push_str(x.remote_user),
            'p' => out.push_str(&x.port.to_string()),
            _ => return None,
        }
    }
    Some(PathBuf::from(out))
}

/// The resolved files that exist, in `ssh`'s order, each once.
pub fn existing_files(r: &Resolved, x: &Expand) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in r.files.iter().filter_map(|f| expand(f, x)) {
        if p.is_file() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// `-G -p <port> -l <user> -- <host>`: the same target the install connects to.
pub fn query_argv(host: &RequestHost, user: &str) -> Vec<OsString> {
    [
        "-G".to_string(),
        "-p".into(),
        host.port().to_string(),
        "-l".into(),
        user.to_string(),
        "--".into(),
        host.connect_host().to_string(),
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// Ask the user's `ssh`. `None` when it cannot answer; the caller falls back
/// to the connection made before K7.
pub fn query(host: &RequestHost, user: &str) -> Option<Resolved> {
    let mut cmd = Command::new(SSH_BINARY);
    cmd.args(query_argv(host, user));
    let out = super::install::run_tool(cmd, SSH_BINARY).ok()?;
    if !out.status.success() {
        tracing::warn!(
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "ssh -G failed; the install falls back to ssh's own key choice"
        );
        return None;
    }
    Some(parse(&String::from_utf8_lossy(&out.stdout), |v| {
        std::env::var(v).ok()
    }))
}
