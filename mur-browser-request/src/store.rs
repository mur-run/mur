//! The request files on disk: the only writer the tool uses, the only reader
//! and the only delete. See the crate doc for why the reader is strict.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use mur_common::pinned_dir::PinnedDir;

use super::{
    BrowserRequest, EXPIRY_SECS, MAX_FILE_BYTES, MAX_PENDING, REQUESTS_DIR, SCHEMA, Site,
    sanitize_reason,
};

/// A request [`read_pending`] accepted. Only it builds one, so [`remove`] can
/// only ever name a validated id.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PendingRequest {
    pub site: Site,
    pub login: bool,
    /// Already sanitized.
    pub reason: String,
    pub requested_at: DateTime<Utc>,
}

/// What [`write`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub site: Site,
    /// A request for the same origin was there and is now replaced.
    pub replaced: bool,
}

pub fn requests_dir(agent_home: &Path) -> PathBuf {
    agent_home.join(REQUESTS_DIR)
}

fn file_name(id: &str) -> String {
    format!("{id}.json")
}

/// Write `req` under its id, replacing an earlier request for the same
/// origin. Temp file + rename, so a reader never sees half a file.
pub fn write(agent_home: &Path, req: &BrowserRequest) -> io::Result<Written> {
    let site = Site::parse(&req.site).map_err(io::Error::other)?;
    let dir = requests_dir(agent_home);
    std::fs::create_dir_all(&dir)?;
    let body = serde_json::to_vec_pretty(req).map_err(io::Error::other)?;
    if body.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::other(
            "the request is larger than a request file may be",
        ));
    }
    let dest = dir.join(file_name(&site.id()));
    let replaced = std::fs::symlink_metadata(&dest).is_ok();
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(&body)?;
    tmp.persist(&dest).map_err(|e| e.error)?;
    Ok(Written { site, replaced })
}

/// Every acceptable request, newest first, at most [`MAX_PENDING`].
/// Anything that fails a check is skipped silently: the files are the
/// agent's, and a bad one must not hide the good ones.
pub fn read_pending(agent_home: &Path, now: DateTime<Utc>) -> Vec<PendingRequest> {
    let Ok(dir) = PinnedDir::open(&requests_dir(agent_home)) else {
        return Vec::new();
    };
    let Ok(names) = dir.names() else {
        return Vec::new();
    };
    let mut out: Vec<PendingRequest> = names
        .iter()
        .filter_map(|name| read_one(&dir, name, now))
        .collect();
    out.sort_by(|a, b| {
        b.requested_at
            .cmp(&a.requested_at)
            .then_with(|| a.site.id().cmp(&b.site.id()))
    });
    out.truncate(MAX_PENDING);
    out
}

fn read_one(dir: &PinnedDir, name: &str, now: DateTime<Utc>) -> Option<PendingRequest> {
    let id = id_of(name)?;
    let f = dir.open_regular(name).ok()??;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    f.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return None;
    }
    let req: BrowserRequest = serde_json::from_slice(&buf).ok()?;
    if req.schema != SCHEMA {
        return None;
    }
    let site = Site::parse(&req.site).ok()?;
    // Canonical form only: a hand-written path or uppercase host is not
    // something the tool would have written.
    if site.origin() != req.site || site.id() != id {
        return None;
    }
    if now.signed_duration_since(req.requested_at) > chrono::Duration::seconds(EXPIRY_SECS) {
        return None;
    }
    let reason = sanitize_reason(&req.reason);
    if reason.is_empty() {
        return None;
    }
    Some(PendingRequest {
        site,
        login: req.login,
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
///
/// Only if the file still holds `p`: [`write`] replaces a request for the
/// same origin, so an agent that asked again while `p`'s card was open has
/// a newer request under the same name, and answering `p` must not delete
/// it unseen. The check reads at `p.requested_at`, so expiry cannot hide a
/// match. (A write landing between the check and the unlink is still lost;
/// that window is microseconds, the one this closes is a whole login.)
pub fn remove(agent_home: &Path, p: &PendingRequest) -> io::Result<()> {
    let dir = PinnedDir::open(&requests_dir(agent_home))?;
    let name = file_name(&p.site.id());
    match read_one(&dir, &name, p.requested_at) {
        Some(on_disk) if on_disk == *p => {}
        Some(_) => return Ok(()),
        // Gone already, or no longer a valid request: deleting it is what
        // the old behaviour did and loses nothing the user could see.
        None => {}
    }
    match dir.unlink(&name) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
