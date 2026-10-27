//! What `running.lock` records about a startup override: the one this boot
//! applied, or why it rejected the one it found. Owned by the lock, so it
//! lives beside `LockFile`; `crate::startup_override` imports `OverrideFlags`.

use serde::{Deserialize, Serialize};

use crate::startup_override::ConfirmedBy;

/// The three one-shot relaxations an override may request.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverrideFlags {
    pub no_hitl: bool,
    pub net_unrestricted: bool,
    pub full_perms: bool,
}

/// An override this boot applied. Its `nonce` is the acknowledgement the
/// writer is waiting for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OverrideRecord {
    pub nonce: String,
    pub flags: OverrideFlags,
    pub confirmed_by: ConfirmedBy,
}

/// An override this boot found and refused. `nonce` is `None` when the file
/// could not be read or parsed, so no trustworthy nonce exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OverrideRejection {
    pub nonce: Option<String>,
    pub reason: String,
}
