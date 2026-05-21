//! `.muragent` v2 portable agent package format.
//!
//! ## Module map
//!
//! - `manifest` — type definitions for `manifest.yaml`
//! - `jcs_canonical` — manifest → `manifest.signed.json` (RFC 8785 JCS via `mur_common::jcs`)
//! - `dsse` — DSSE envelope sign/verify
//! - `statement` — in-toto v1 Statement with subject hashes
//! - `writer` — build `.muragent` tar.gz
//! - `reader` — extract and validate `.muragent` tar.gz
//! - `validator` — 11-step validation pipeline
//! - `executable_ban` — MCP command deny-list and permit-list

pub mod dsse;
pub mod executable_ban;
pub mod jcs_canonical;
pub mod manifest;
pub mod reader;
pub mod statement;
pub mod validator;
pub mod writer;
// pub mod statement; — Task 6
// pub mod executable_ban; — Task 7
// pub mod writer; — Task 8
// pub mod reader; — Task 9
// pub mod validator; — Task 9

use thiserror::Error;

#[derive(Error, Debug)]
pub enum MuragentError {
    #[error("schema version mismatch: expected 'mur-agent/2', got '{0}'")]
    SchemaMismatch(String),

    #[error("manifest YAML parse error: {0}")]
    ManifestParse(String),

    #[error("manifest.signed.json mismatch: re-derived canonical JSON does not match embedded")]
    SignedJsonMismatch,

    #[error("DSSE verification failed: {0}")]
    DsseError(String),

    #[error("subject hash mismatch for '{path}': expected {expected}, got {actual}")]
    SubjectHashMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("missing subject in tarball: '{0}'")]
    MissingSubject(String),

    #[error("extra file in tarball not in statement subjects: '{0}'")]
    ExtraFile(String),

    #[error("executable content detected: {0}")]
    ExecutableContent(String),

    #[error("forbidden MCP command: {0}")]
    ForbiddenMcpCommand(String),

    #[error("signature invalid for keyid '{0}'")]
    InvalidSignature(String),

    #[error("trust refused: {0}")]
    TrustRefused(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}
