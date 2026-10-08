//! Signed push request, written by the agent-side `git_push_request` tool into
//! `<agent_home>/inbox/git-push/<request_id>.yaml` (pack alongside as
//! `<request_id>.pack`) and verified by the daemon sweeper (spec §2).
//!
//! Precedent: `mur_track::snapshot_request` — YAML, tmp+rename, domain-tagged
//! sign-input with `sig` excluded. The agent id the daemon acts on comes from the
//! directory the file was found in, never from the file: `verify_request` refuses
//! a file whose `agent` field disagrees with that directory.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use mur_common::identity::verify_bytes;

/// Domain tag; bump on any change to the field list or order.
pub const SIGN_DOMAIN: &str = "mur-git-push-request-v1\n";
/// Longest accepted id (agent, task, request, repo, remote).
pub const MAX_ID_LEN: usize = 128;
pub const REQUEST_EXT: &str = "yaml";
pub const PACK_EXT: &str = "pack";
const TMP_EXT: &str = "tmp";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestFile {
    pub agent: String,
    pub task_id: String,
    pub request_id: String,
    pub repo_id: String,
    pub remote_id: String,
    pub r#ref: String,
    pub old_sha: String,
    pub new_sha: String,
    pub pack_sha256: String,
    pub requested_at: DateTime<Utc>,
    pub key_version: u32,
    /// Multibase Ed25519 signature over [`RequestFile::sign_input`].
    pub sig: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RequestFileError {
    #[error("invalid id: {0}")]
    InvalidId(&'static str),
    #[error("io: {0}")]
    Io(String),
    #[error("malformed request file: {0}")]
    Malformed(String),
    #[error("bad signature")]
    BadSignature,
    /// The file names an agent other than the directory it was found in.
    #[error("agent mismatch")]
    AgentMismatch,
    /// The file name and `request_id` disagree.
    #[error("request id mismatch")]
    NameMismatch,
    /// A different request already holds this `request_id`.
    #[error("request_conflict")]
    Conflict,
}

impl From<std::io::Error> for RequestFileError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// A name, never a path: 1..=[`MAX_ID_LEN`] of `[A-Za-z0-9._-]`, not starting with `.`.
pub fn validate_id(s: &str, what: &'static str) -> Result<(), RequestFileError> {
    let ok = !s.is_empty()
        && s.len() <= MAX_ID_LEN
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(RequestFileError::InvalidId(what))
    }
}

impl RequestFile {
    fn ids(&self) -> [(&str, &'static str); 5] {
        [
            (self.agent.as_str(), "agent"),
            (self.task_id.as_str(), "task_id"),
            (self.request_id.as_str(), "request_id"),
            (self.repo_id.as_str(), "repo_id"),
            (self.remote_id.as_str(), "remote_id"),
        ]
    }

    pub fn validate_ids(&self) -> Result<(), RequestFileError> {
        self.ids()
            .into_iter()
            .try_for_each(|(v, w)| validate_id(v, w))
    }

    /// Domain tag + every field except `sig`, one per line.
    pub fn sign_input(&self) -> Vec<u8> {
        let fields = [
            self.agent.clone(),
            self.task_id.clone(),
            self.request_id.clone(),
            self.repo_id.clone(),
            self.remote_id.clone(),
            self.r#ref.clone(),
            self.old_sha.clone(),
            self.new_sha.clone(),
            self.pack_sha256.clone(),
            self.requested_at.to_rfc3339(),
            self.key_version.to_string(),
        ];
        format!("{SIGN_DOMAIN}{}", fields.join("\n")).into_bytes()
    }

    /// Fail-closed. A field carrying a newline could shift the line framing, so
    /// it fails outright rather than being signed ambiguously.
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        let framed = [
            &self.agent,
            &self.task_id,
            &self.request_id,
            &self.repo_id,
            &self.remote_id,
            &self.r#ref,
            &self.old_sha,
            &self.new_sha,
            &self.pack_sha256,
        ];
        if framed.iter().any(|f| f.contains('\n')) {
            return false;
        }
        verify_bytes(pubkey, &self.sign_input(), &self.sig)
    }

    /// The pack on disk still hashes to the signed digest.
    pub fn verify_pack(&self, pack: &Path) -> bool {
        sha256_file(pack).is_ok_and(|d| d == self.pack_sha256)
    }

    /// Same request as `other` for idempotency: identical agent-chosen content.
    pub fn same_request_as(&self, other: &RequestFile) -> bool {
        (
            &self.agent,
            &self.request_id,
            &self.repo_id,
            &self.remote_id,
            &self.r#ref,
            &self.old_sha,
            &self.new_sha,
        ) == (
            &other.agent,
            &other.request_id,
            &other.repo_id,
            &other.remote_id,
            &other.r#ref,
            &other.old_sha,
            &other.new_sha,
        )
    }
}

pub fn sha256_file(path: &Path) -> Result<String, RequestFileError> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(hex::encode(h.finalize()))
}

pub fn request_path(dir: &Path, request_id: &str) -> PathBuf {
    dir.join(format!("{request_id}.{REQUEST_EXT}"))
}

pub fn pack_path(dir: &Path, request_id: &str) -> PathBuf {
    dir.join(format!("{request_id}.{PACK_EXT}"))
}

/// Read a request file without verifying it.
pub fn read_request(path: &Path) -> Result<RequestFile, RequestFileError> {
    let text = std::fs::read_to_string(path)?;
    serde_yaml_ng::from_str(&text).map_err(|e| RequestFileError::Malformed(e.to_string()))
}

/// Atomic (tmp + rename) and idempotent per `request_id`: an identical request
/// already on disk is left alone; a different one is [`RequestFileError::Conflict`].
/// Ids are checked before anything touches the filesystem.
pub fn write_request(dir: &Path, r: &RequestFile) -> Result<PathBuf, RequestFileError> {
    r.validate_ids()?;
    let dest = request_path(dir, &r.request_id);
    if dest.exists() {
        return if read_request(&dest)?.same_request_as(r) {
            Ok(dest)
        } else {
            Err(RequestFileError::Conflict)
        };
    }
    std::fs::create_dir_all(dir)?;
    let yaml =
        serde_yaml_ng::to_string(r).map_err(|e| RequestFileError::Malformed(e.to_string()))?;
    let tmp = dir.join(format!("{}.{REQUEST_EXT}.{TMP_EXT}", r.request_id));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(yaml.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &dest)?;
    Ok(dest)
}

/// Daemon side. `dir_agent` is the agent whose inbox `path` was found in and
/// `pubkey` that agent's on-disk key; the returned request's `agent` equals it.
pub fn verify_request(
    path: &Path,
    dir_agent: &str,
    pubkey: &[u8; 32],
) -> Result<RequestFile, RequestFileError> {
    let r = read_request(path)?;
    r.validate_ids()?;
    if path.file_stem().and_then(|s| s.to_str()) != Some(r.request_id.as_str()) {
        return Err(RequestFileError::NameMismatch);
    }
    if !r.verify(pubkey) {
        return Err(RequestFileError::BadSignature);
    }
    if r.agent != dir_agent {
        return Err(RequestFileError::AgentMismatch);
    }
    Ok(r)
}

#[cfg(test)]
#[path = "request_file_tests.rs"]
mod tests;
