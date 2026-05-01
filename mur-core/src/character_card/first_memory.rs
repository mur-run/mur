//! `FirstMemoryExt` — the payload of the `extensions.mur.first_memory`
//! field in a `.murcard.yaml`. Carries the user-supplied first memory text
//! plus the timestamp at which onboarding established it.
//!
//! This is the on-disk projection of `mur_common::agent::FirstMemory`. The
//! two are intentionally separate types so the card schema can evolve
//! independently of the live agent profile.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FirstMemoryExt {
    pub text: String,
    pub established_at: chrono::DateTime<chrono::Utc>,
}
