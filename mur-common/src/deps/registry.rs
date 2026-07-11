//! MUR-curated program registry. URLs + pinned SHA-256 are MUR-owned, so a
//! shared bundle can only *reference* a key — it cannot substitute the source.

use serde::Deserialize;
use std::collections::BTreeMap;

const MANIFEST: &str = include_str!("registry_manifest.yaml");

#[derive(Debug, Clone, Deserialize)]
pub struct CuratedRecipe {
    #[serde(default)]
    pub description: String,
    pub url: String,
    pub sha256: String,
    /// For a bare-binary recipe; `None` when `archive` is set.
    #[serde(default)]
    pub install_to: Option<String>,
    #[serde(default)]
    pub executable: bool,
    /// For a multi-file tarball (e.g. obscura's two binaries).
    #[serde(default)]
    pub archive: Option<ArchiveSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArchiveSpec {
    pub members: Vec<RecipeMember>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecipeMember {
    pub path_in_archive: String,
    pub install_to: String,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(default)]
    description: String,
    platforms: BTreeMap<String, PlatformRaw>,
}

#[derive(Debug, Deserialize)]
struct PlatformRaw {
    url: String,
    sha256: String,
    #[serde(default)]
    install_to: Option<String>,
    #[serde(default)]
    executable: bool,
    #[serde(default)]
    archive: Option<ArchiveSpec>,
}

fn load() -> BTreeMap<String, Entry> {
    serde_yaml::from_str(MANIFEST).expect("embedded registry_manifest.yaml is valid")
}

/// Resolve a curated recipe for `key` on `platform` (`<arch>-<os>`).
pub fn recipe(key: &str, platform: &str) -> Option<CuratedRecipe> {
    let map = load();
    let entry = map.get(key)?;
    let p = entry.platforms.get(platform)?;
    Some(CuratedRecipe {
        description: entry.description.clone(),
        url: p.url.clone(),
        sha256: p.sha256.clone(),
        install_to: p.install_to.clone(),
        executable: p.executable,
        archive: p.archive.clone(),
    })
}

/// True if `key` names a curated program (on any platform).
pub fn is_curated(key: &str) -> bool {
    load().contains_key(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lightpanda_recipe_resolves_per_platform() {
        let r = recipe("lightpanda", "aarch64-macos");
        assert!(r.is_some(), "lightpanda/aarch64-macos should exist");
        let r = r.unwrap();
        assert!(!r.url.is_empty() && r.sha256.len() == 64);
        assert!(
            r.install_to
                .as_ref()
                .map_or(false, |p| p.starts_with("aura/"))
        );
        assert!(is_curated("lightpanda"));
        assert!(!is_curated("totally-unknown-key"));
        assert!(recipe("lightpanda", "sparc-solaris").is_none());
    }

    #[test]
    fn obscura_recipe_is_an_archive_with_two_members() {
        let r = recipe("obscura", "aarch64-macos").expect("obscura/aarch64-macos");
        let a = r.archive.expect("obscura ships an archive");
        assert_eq!(a.members.len(), 2, "obscura + obscura-worker");
        assert!(a.members.iter().any(|m| m.install_to == "aura/obscura"));
        assert!(
            a.members
                .iter()
                .any(|m| m.install_to == "aura/obscura-worker")
        );
    }
}
