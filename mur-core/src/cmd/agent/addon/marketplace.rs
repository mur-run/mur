//! Claude plugin **marketplace** support: a `.claude-plugin/marketplace.json`
//! that indexes many plugins. `mur agent addon import <agent> <marketplace>
//! --plugin <name>` clones the marketplace, finds the named plugin, resolves
//! its source (a relative path inside the marketplace repo, or a separate
//! `url`/`github`/`git-subdir` repo cloned into the addon cache), and imports
//! that directory through the normal local-dir importer.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct MarketplaceManifest {
    #[serde(default)]
    pub plugins: Vec<MarketplacePlugin>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarketplacePlugin {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub source: PluginSource,
}

/// A plugin's location: a relative path inside the marketplace repo, or a
/// separate repo to clone.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PluginSource {
    /// e.g. `"./plugins/foo"` or `"./"`.
    Path(String),
    /// e.g. `{"source":"url","url":"…"}` / `{"source":"git-subdir","url":"…","path":"…"}`.
    Remote(RemotePluginSource),
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemotePluginSource {
    pub source: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// Parse a `.claude-plugin/marketplace.json`.
pub fn parse_marketplace(content: &str) -> Result<MarketplaceManifest> {
    Ok(serde_json::from_str(content)?)
}

/// Find a plugin entry by name.
pub fn find_plugin<'a>(m: &'a MarketplaceManifest, name: &str) -> Option<&'a MarketplacePlugin> {
    m.plugins.iter().find(|p| p.name == name)
}

/// Resolve a plugin's on-disk directory under `marketplace_root`, cloning a
/// remote source into `addon_cache` if needed.
pub fn resolve_plugin_dir(
    plugin: &MarketplacePlugin,
    marketplace_root: &Path,
    addon_cache: &Path,
) -> Result<PathBuf> {
    match &plugin.source {
        PluginSource::Path(rel) => {
            let rel = rel.trim_start_matches("./").trim_start_matches('/');
            Ok(marketplace_root.join(rel))
        }
        PluginSource::Remote(r) => {
            let clone_url = remote_clone_url(r)?;
            let key = super::import::cache_key_for(&clone_url);
            let dest = addon_cache.join(key);
            crate::cmd::skill_registry::git_clone_or_pull(&clone_url, &dest)?;
            match &r.path {
                Some(p) => Ok(dest.join(p.trim_start_matches("./").trim_start_matches('/'))),
                None => Ok(dest),
            }
        }
    }
}

fn remote_clone_url(r: &RemotePluginSource) -> Result<String> {
    if let Some(url) = &r.url {
        return Ok(url.clone());
    }
    if let Some(repo) = &r.repo {
        return Ok(format!("https://github.com/{repo}.git"));
    }
    anyhow::bail!("plugin source '{}' has no url/repo to clone", r.source)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "name": "m",
      "plugins": [
        {"name": "a", "description": "da", "source": "./plugins/a"},
        {"name": "b", "source": {"source": "url", "url": "https://github.com/o/b.git"}},
        {"name": "c", "source": {"source": "git-subdir", "url": "https://github.com/o/mono.git", "path": "plugins/c"}}
      ]
    }"#;

    #[test]
    fn parse_find_and_resolve_relative_path() {
        let m = parse_marketplace(SAMPLE).unwrap();
        assert_eq!(m.plugins.len(), 3);
        assert_eq!(find_plugin(&m, "a").unwrap().name, "a");
        assert!(find_plugin(&m, "nope").is_none());

        // A relative-path source resolves under the marketplace root, no clone.
        let dir = resolve_plugin_dir(
            find_plugin(&m, "a").unwrap(),
            Path::new("/mkt"),
            Path::new("/cache"),
        )
        .unwrap();
        assert_eq!(dir, PathBuf::from("/mkt/plugins/a"));

        // The remote variants parse into Remote with their fields.
        assert!(matches!(
            find_plugin(&m, "b").unwrap().source,
            PluginSource::Remote(ref r) if r.source == "url"
        ));
        assert!(matches!(
            find_plugin(&m, "c").unwrap().source,
            PluginSource::Remote(ref r) if r.path.as_deref() == Some("plugins/c")
        ));
    }
}
