//! The manifest gate (§5): a build must refuse a channel it cannot read
//! correctly instead of rewriting it. Three states, never conflated: no
//! directory (the caller's UNKNOWN_CONVERSATION), `v` above the build
//! (TooNew), log-without-manifest or unparseable manifest (Missing /
//! Unparseable). `ensure_manifest` used to write a placeholder with the
//! current build's `v` whenever the file was absent, which let a v2 build
//! stamp `v: 2` onto a v3 channel whose manifest was lost.
use std::fmt;
use std::fs;

use anyhow::{Context, Result};
use mur_common::channel::{CHANNEL_SCHEMA_VERSION, Channel, ChannelActor};
use serde::Deserialize;

use super::ChannelStore;

/// Why a channel's manifest cannot be used. Carried inside `anyhow::Error`;
/// callers branch with `err.downcast_ref::<ManifestError>()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The directory has an events file but no manifest.
    Missing,
    /// The manifest file exists but does not parse (not even its `v`).
    Unparseable(String),
    /// The manifest `v` is above this build's `CHANNEL_SCHEMA_VERSION`.
    TooNew { found: u32, supported: u32 },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "channel has events but no manifest"),
            Self::Unparseable(e) => write!(f, "channel manifest does not parse: {e}"),
            Self::TooNew { found, supported } => write!(
                f,
                "channel manifest is schema v{found}; this build reads up to v{supported}"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

/// How `with_append_lock` treats a channel whose manifest is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestPolicy {
    /// Ordinary appends: log-without-manifest is `ManifestError::Missing`.
    Require,
    /// `restart_chain` only: log-without-manifest gets a placeholder, as the
    /// old `ensure_manifest` wrote — the repair is the one operation whose
    /// job is to put a damaged channel back into a known state.
    Restore,
}

/// Stage one of the two-stage parse: only `v`, so a newer manifest with
/// fields this build does not know is reported as TooNew, not Unparseable.
#[derive(Deserialize)]
struct VersionOnly {
    v: u32,
}

/// Parse `yaml` as a manifest: TooNew is decided before the full parse.
pub(super) fn parse_manifest(yaml: &str) -> Result<Channel> {
    let head: VersionOnly =
        serde_yaml::from_str(yaml).map_err(|e| ManifestError::Unparseable(e.to_string()))?;
    if head.v > CHANNEL_SCHEMA_VERSION {
        return Err(ManifestError::TooNew {
            found: head.v,
            supported: CHANNEL_SCHEMA_VERSION,
        }
        .into());
    }
    serde_yaml::from_str(yaml).map_err(|e| ManifestError::Unparseable(e.to_string()).into())
}

impl ChannelStore {
    /// Manifest state of `id`. The caller holds the append lock (exclusive)
    /// so a manifest written by a racing repair is seen. Under `Restore`, an
    /// absent manifest is written as a placeholder; under `Require` it is
    /// `Missing`. Unparseable and TooNew are errors under both.
    pub(crate) fn check_manifest_locked(&self, id: &str, policy: ManifestPolicy) -> Result<()> {
        let path = self.manifest_path(id)?;
        match fs::read_to_string(&path) {
            Ok(s) => parse_manifest(&s).map(|_| ()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match policy {
                ManifestPolicy::Require => Err(ManifestError::Missing.into()),
                ManifestPolicy::Restore => self.write_placeholder(id),
            },
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// The placeholder the old `ensure_manifest` wrote: honest about what it
    /// knows — the id, the schema version, and that nobody claimed ownership.
    /// Title and participants fill in through the normal paths.
    fn write_placeholder(&self, id: &str) -> Result<()> {
        let now = chrono::Utc::now();
        self.write_manifest_file(&Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: id.to_string(),
            title: id.to_string(),
            goal: mur_common::channel::Goal::default(),
            state: mur_common::channel::ChannelState::Working,
            purpose: None,
            owner: ChannelActor::System,
            participants: vec![],
            created_at: now,
            updated_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_new_wins_over_unknown_fields() {
        let yaml = format!(
            "v: {}\nid: c1\ntitle: t\nfuture_field: {{nested: true}}\n",
            CHANNEL_SCHEMA_VERSION + 1
        );
        let err = parse_manifest(&yaml).unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<ManifestError>(),
                Some(ManifestError::TooNew { .. })
            ),
            "{err}"
        );
    }

    #[test]
    fn garbage_is_unparseable() {
        let err = parse_manifest(": : :").unwrap_err();
        assert!(matches!(
            err.downcast_ref::<ManifestError>(),
            Some(ManifestError::Unparseable(_))
        ));
    }
}
