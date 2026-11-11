//! The request files on disk: the only writer the tool uses, the only reader
//! and the only delete. See the module doc for why the reader is strict.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::{
    EXPIRY_SECS, MAX_FILE_BYTES, MAX_PENDING, REQUESTS_DIR, RequestHost, SCHEMA, SshRequest,
    sanitize_reason, validate_user,
};

/// A request [`read_pending`] accepted. Only it builds one, so [`remove`] can
/// only ever name a validated id.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PendingRequest {
    pub target: RequestHost,
    pub user: Option<String>,
    /// Already sanitized.
    pub reason: String,
    pub requested_at: DateTime<Utc>,
}

/// What [`write`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub target: RequestHost,
    /// A request for the same `host:port` was there and is now replaced.
    pub replaced: bool,
}

pub fn requests_dir(agent_home: &Path) -> PathBuf {
    agent_home.join(REQUESTS_DIR)
}

fn file_name(id: &str) -> String {
    format!("{id}.json")
}

/// Write `req` under its id, replacing an earlier request for the same host.
/// Temp file + rename, so a reader never sees half a file.
pub fn write(agent_home: &Path, req: &SshRequest) -> io::Result<Written> {
    let target = RequestHost::parse(&req.host, Some(req.port)).map_err(io::Error::other)?;
    let dir = requests_dir(agent_home);
    std::fs::create_dir_all(&dir)?;
    let body = serde_json::to_vec_pretty(req).map_err(io::Error::other)?;
    if body.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::other(
            "the request is larger than a request file may be",
        ));
    }
    let dest = dir.join(file_name(&target.id()));
    let replaced = std::fs::symlink_metadata(&dest).is_ok();
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(&body)?;
    tmp.persist(&dest).map_err(|e| e.error)?;
    Ok(Written { target, replaced })
}

/// Every acceptable request, newest first, at most [`MAX_PENDING`].
/// Anything that fails a check is skipped silently: the files are the
/// agent's, and a bad one must not hide the good ones.
pub fn read_pending(agent_home: &Path, now: DateTime<Utc>) -> Vec<PendingRequest> {
    let dir = requests_dir(agent_home);
    // A symlinked directory would make this read, and `remove` delete, elsewhere.
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PendingRequest> = entries
        .flatten()
        .filter_map(|e| read_one(&dir, e.file_name().to_str()?, now))
        .collect();
    out.sort_by(|a, b| {
        b.requested_at
            .cmp(&a.requested_at)
            .then_with(|| a.target.id().cmp(&b.target.id()))
    });
    out.truncate(MAX_PENDING);
    out
}

fn read_one(dir: &Path, name: &str, now: DateTime<Utc>) -> Option<PendingRequest> {
    let id = id_of(name)?;
    let f = mur_common::paths::open_no_follow(&dir.join(name)).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    let mut limited = f.take(MAX_FILE_BYTES + 1);
    limited.read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return None;
    }
    let req: SshRequest = serde_json::from_slice(&buf).ok()?;
    if req.schema != SCHEMA {
        return None;
    }
    let target = RequestHost::parse(&req.host, Some(req.port)).ok()?;
    if target.id() != id {
        return None;
    }
    if let Some(u) = &req.user {
        validate_user(u).ok()?;
    }
    if now.signed_duration_since(req.requested_at) > chrono::Duration::seconds(EXPIRY_SECS) {
        return None;
    }
    let reason = sanitize_reason(&req.reason);
    if reason.is_empty() {
        return None;
    }
    Some(PendingRequest {
        target,
        user: req.user,
        reason,
        requested_at: req.requested_at,
    })
}

/// `name` is exactly 64 lowercase hex characters followed by `.json`.
fn id_of(name: &str) -> Option<&str> {
    let id = name.strip_suffix(".json")?;
    (id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(id)
}

/// Delete the file of a request [`read_pending`] returned. Unlinks the entry
/// itself (never what a link points to); already gone is success.
pub fn remove(agent_home: &Path, p: &PendingRequest) -> io::Result<()> {
    let dir = requests_dir(agent_home);
    if !std::fs::symlink_metadata(&dir)?.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    match std::fs::remove_file(dir.join(file_name(&p.target.id()))) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
