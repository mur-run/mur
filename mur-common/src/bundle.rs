//! Wire-format types shared between the GUI export pipeline (writer)
//! and the GUI app's first-launch bootstrap (reader).
//!
//! Both halves of `mur agent export --format gui` need to agree on
//! these structs byte-for-byte: the export pipeline writes
//! `metadata.json` into the bundled resources, and the bootstrap
//! reads it back. Keeping them in `mur-common` (the shared schema
//! crate) is the only way to prevent silent drift if either side
//! adds or renames a field.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BundleMode {
    /// Recipient mints fresh identity on first launch (default; safe
    /// for distribution).
    Template,
    /// Recipient inherits the source agent's identity. Currently
    /// gated behind `MUR_ALLOW_UNSAFE_CLONE` because the rekey
    /// ceremony is not yet wired.
    Clone,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddedMetadata {
    pub schema_version: u32,
    pub agent_name: String,
    pub display_name: String,
    pub mode: BundleMode,
    pub theme_default: String,
    pub mur_version: String,
}
