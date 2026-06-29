use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Request sent from mur-core (host) to mur-zfs-agent (inside VM).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ZfsRequest {
    CreateTrack { base: PathBuf, name: String },
    Snapshot { track: PathBuf, label: String },
    DiffFiles { track: PathBuf, since: String },
    Destroy { track: PathBuf },
}

/// Response sent from mur-zfs-agent back to mur-core.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ZfsResponse {
    /// CreateTrack succeeded — `path` is the mountpoint of the new clone.
    Track { path: PathBuf },
    /// Snapshot succeeded — `snap_id` is the full snapshot name (e.g. `pool/ds@label`).
    Snap { snap_id: String },
    /// DiffFiles succeeded — `paths` are repo-relative changed paths.
    Files { paths: Vec<PathBuf> },
    /// Operation succeeded with no output (Destroy).
    Ok,
    /// Operation failed — `message` is the error string.
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_track_roundtrip() {
        let req = ZfsRequest::CreateTrack {
            base: "/pool/data/project".into(),
            name: "track-a".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ZfsRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ZfsRequest::CreateTrack { .. }));
    }

    #[test]
    fn error_response_roundtrip() {
        let resp = ZfsResponse::Error {
            message: "zfs: command not found".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ZfsResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ZfsResponse::Error { .. }));
    }

    #[test]
    fn all_request_variants_serialize() {
        let reqs = vec![
            ZfsRequest::CreateTrack {
                base: "/p".into(),
                name: "t".into(),
            },
            ZfsRequest::Snapshot {
                track: "/p/t".into(),
                label: "base".into(),
            },
            ZfsRequest::DiffFiles {
                track: "/p/t".into(),
                since: "mur-base".into(),
            },
            ZfsRequest::Destroy {
                track: "/p/t".into(),
            },
        ];
        for req in reqs {
            let json = serde_json::to_string(&req).unwrap();
            let _back: ZfsRequest = serde_json::from_str(&json).unwrap();
        }
    }
}
