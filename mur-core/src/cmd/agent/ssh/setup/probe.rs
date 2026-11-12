//! The outside world `plan` reads: the user's ssh-agent, `~/.ssh/*.pub`,
//! one host scan and the user's own `known_hosts`. Behind a trait so the
//! plan is tested without a network or an ssh-agent.

use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use mur_common::agent::fingerprint_of_base64;
use mur_ssh_request::RequestHost;

use super::super::resolve::{self, SSH_ADD_BIN, ScannedKey, parse_keyscan};

/// Looks up the user's own `known_hosts` entries.
pub const KEYGEN_BIN: &str = "ssh-keygen";
/// Where the user's own ssh client files live, under `$HOME`.
const USER_SSH_DIR: &str = ".ssh";
const USER_KNOWN_HOSTS: &str = "known_hosts";
const PUB_SUFFIX: &str = ".pub";

/// One key the user could grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentKey {
    pub fingerprint: String,
    pub comment: String,
    /// The `.pub` file, for a key not loaded yet.
    pub path: Option<PathBuf>,
}

/// The private key `ssh-add` must be given for a `.pub` file: `ssh-add`
/// reads its argument as a private key, so handing it the `.pub` itself fails
/// ("UNPROTECTED PRIVATE KEY FILE") and nothing is loaded.
pub fn private_key_of(pub_path: &std::path::Path) -> PathBuf {
    let s = pub_path.to_string_lossy();
    match s.strip_suffix(PUB_SUFFIX) {
        Some(stem) => PathBuf::from(stem),
        None => pub_path.to_path_buf(),
    }
}

pub trait Probe {
    /// Keys loaded in the user's ssh-agent; empty when there is none.
    fn agent_keys(&self) -> Vec<AgentKey>;
    /// `~/.ssh/*.pub`.
    fn unloaded_keys(&self) -> Vec<AgentKey>;
    fn scan(&self, host: &RequestHost) -> Result<Vec<ScannedKey>>;
    /// Fingerprints the user's `known_hosts` stores for `host`.
    fn stored_host_keys(&self, host: &RequestHost) -> Vec<String>;
    fn user_known_hosts(&self) -> Option<PathBuf>;
}

/// The real machine.
pub struct System;

fn parse_pub_line(line: &str, path: Option<PathBuf>) -> Option<AgentKey> {
    let mut f = line.trim().splitn(3, ' ');
    let (_ty, b64) = (f.next()?, f.next()?);
    Some(AgentKey {
        fingerprint: fingerprint_of_base64(b64)?,
        comment: f.next().unwrap_or("").trim().to_string(),
        path,
    })
}

impl Probe for System {
    fn agent_keys(&self) -> Vec<AgentKey> {
        let Ok(out) = Command::new(SSH_ADD_BIN).arg("-L").output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| parse_pub_line(l, None))
            .collect()
    }

    fn unloaded_keys(&self) -> Vec<AgentKey> {
        let Some(dir) = dirs::home_dir().map(|h| h.join(USER_SSH_DIR)) else {
            return Vec::new();
        };
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut keys: Vec<AgentKey> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().ends_with(PUB_SUFFIX) && p.is_file())
            .filter_map(|p| {
                let line = std::fs::read_to_string(&p).ok()?;
                parse_pub_line(line.lines().next()?, Some(p))
            })
            .collect();
        keys.sort_by(|a, b| a.path.cmp(&b.path));
        keys
    }

    fn scan(&self, host: &RequestHost) -> Result<Vec<ScannedKey>> {
        resolve::scan_host(host.connect_host(), host.port())
    }

    fn stored_host_keys(&self, host: &RequestHost) -> Vec<String> {
        let Some(kh) = self.user_known_hosts() else {
            return Vec::new();
        };
        // Exit 1 is "no entry", 255 is "no file": both read as no entry.
        let Ok(out) = Command::new(KEYGEN_BIN)
            .arg("-F")
            .arg(host.known_hosts_name())
            .arg("-f")
            .arg(&kh)
            .output()
        else {
            return Vec::new();
        };
        parse_keyscan(&String::from_utf8_lossy(&out.stdout))
            .into_iter()
            .map(|k| k.fingerprint)
            .collect()
    }

    fn user_known_hosts(&self) -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(USER_SSH_DIR).join(USER_KNOWN_HOSTS))
    }
}
