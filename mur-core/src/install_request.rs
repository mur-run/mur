//! Records `install_request` commands relayed from mur-server (Dashboard
//! "Install to Hub" button) so the Hub GUI can live-tail them and show a
//! consent modal (Task 3). Fail-closed: this module only *records*, it never
//! installs anything.
//!
//! Line format written to `<mur_home>/hub/install-requests.jsonl` (append-only,
//! one JSON object per line, same directory family as the mobile-events tail
//! the Hub already watches):
//!
//! ```json
//! {"kind":"install_request","type":"skill","id":"pub/name","requested_at":1234567890,"request_id":"<uuid>"}
//! ```
//!
//! Dedup is by `request_id`: replaying the same relay command (e.g. after a
//! reconnect) is a no-op, not a duplicate row.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// The four installable kinds the Dashboard can request. Kept in sync with
/// mur-server's `internal/api/handlers/install_request.go` whitelist — the
/// server already validates this, but the daemon re-validates in depth.
const ALLOWED_TYPES: &[&str] = &["skill", "mcp", "workflow", "plugin"];

/// A parsed `install_request` relay command, ready to be recorded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallRequest {
    /// The relay command's outer envelope id — used for dedup and for the
    /// ack `result` frame sent back to the relay.
    pub request_id: String,
    /// The item type: one of `skill|mcp|workflow|plugin`. Named `install_type`
    /// to avoid colliding with the `type` keyword; (de)serializes as `"type"`
    /// on the wire so it matches the relay command's `params.type` field.
    #[serde(rename = "type")]
    pub install_type: String,
    /// The item id, e.g. `mur-official/brainstorming`.
    pub id: String,
}

/// One line of `<mur_home>/hub/install-requests.jsonl`.
///
/// Public so the Hub GUI (`install_inbox.rs`) can parse the jsonl
/// directly when listing pending requests for the consent modal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRequestRecord {
    pub kind: String,
    #[serde(rename = "type")]
    pub install_type: String,
    pub id: String,
    pub requested_at: u64,
    pub request_id: String,
}

fn install_requests_path(mur_home: &Path) -> PathBuf {
    mur_home.join("hub").join("install-requests.jsonl")
}

/// Returns true if `request_id` already has a recorded line in `path`.
fn already_recorded(path: &Path, request_id: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<InstallRequestRecord>(&line)
            && rec.request_id == request_id
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Appends one `install_request` line to `<mur_home>/hub/install-requests.jsonl`,
/// creating the `hub` directory if needed. Idempotent by `request_id`: if the
/// same request was already recorded, this is a no-op (still returns the path).
///
/// Rejects (returns `Err`) any `install_type` outside the four-value
/// whitelist — defense in depth, since mur-server already validated it.
pub fn record_install_request(mur_home: &Path, req: &InstallRequest) -> Result<PathBuf> {
    if !ALLOWED_TYPES.contains(&req.install_type.as_str()) {
        bail!(
            "install_request: unsupported type {:?} (expected one of {:?})",
            req.install_type,
            ALLOWED_TYPES
        );
    }

    let path = install_requests_path(mur_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if already_recorded(&path, &req.request_id)? {
        return Ok(path);
    }

    let requested_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let record = InstallRequestRecord {
        kind: "install_request".to_string(),
        install_type: req.install_type.clone(),
        id: req.id.clone(),
        requested_at,
        request_id: req.request_id.clone(),
    };

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let line = serde_json::to_string(&record)?;
    writeln!(file, "{line}")?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(request_id: &str, install_type: &str, id: &str) -> InstallRequest {
        InstallRequest {
            request_id: request_id.to_string(),
            install_type: install_type.to_string(),
            id: id.to_string(),
        }
    }

    #[test]
    fn appends_one_line_and_round_trips_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let r = req("req-1", "skill", "pub/name");

        let path = record_install_request(home, &r).unwrap();
        assert_eq!(path, home.join("hub").join("install-requests.jsonl"));

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1);

        let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(value["kind"], "install_request");
        assert_eq!(value["type"], "skill");
        assert_eq!(value["id"], "pub/name");
        assert_eq!(value["request_id"], "req-1");
        assert!(value["requested_at"].as_u64().unwrap() > 0);
    }

    #[test]
    fn same_request_id_twice_appends_once() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let r = req("req-dup", "mcp", "pub/tool");

        record_install_request(home, &r).unwrap();
        record_install_request(home, &r).unwrap();

        let path = home.join("hub").join("install-requests.jsonl");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }

    #[test]
    fn rejects_invalid_type_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let r = req("req-bad", "exe", "pub/name");

        let result = record_install_request(home, &r);
        assert!(result.is_err());

        let path = home.join("hub").join("install-requests.jsonl");
        assert!(!path.exists());
    }
}
