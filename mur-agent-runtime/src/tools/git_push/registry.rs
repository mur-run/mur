//! `<mur_home>/git-push/registry.yaml`: `repo_id` → the agent's sandbox repo and
//! the remotes a human enrolled for it. Daemon-written; read here on every call so
//! an enrollment change applies without restarting the agent.
//!
//! No credential and no URL lives here — remote endpoints and auth stay with the
//! broker. Lookups answer only "is this id known"; nothing here can list the
//! entries back to the model (premise 2).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEntry {
    pub path: PathBuf,
    #[serde(default)]
    pub remotes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitPushRegistry {
    #[serde(default)]
    repos: BTreeMap<String, RegistryEntry>,
}

impl GitPushRegistry {
    pub fn from_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (S, RegistryEntry)>,
        S: Into<String>,
    {
        Self {
            repos: entries.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        }
    }

    /// Missing file ⇒ empty registry (nothing enrolled). Unreadable or malformed
    /// ⇒ error: a registry that exists but cannot be parsed must not silently
    /// read as "no repos".
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_yaml_ng::from_str(&text).map_err(|e| format!("registry unreadable: {e}"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("registry unreadable: {}", e.kind())),
        }
    }

    pub fn get(&self, repo_id: &str) -> Option<&RegistryEntry> {
        self.repos.get(repo_id)
    }
}
