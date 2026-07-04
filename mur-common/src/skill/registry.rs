//! Git-based skill registry data model for index.yaml.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryIndex {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub skills: BTreeMap<String, RegistrySkillEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistrySkillEntry {
    pub latest: String,
    pub description: String,
    pub publisher: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default)]
    pub content_sha256: String,
    #[serde(default)]
    pub install_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recommended_roles: Vec<String>,
}

impl RegistryIndex {
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml_ng::Error> {
        serde_yaml_ng::from_str(yaml)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml_ng::Error> {
        serde_yaml_ng::to_string(self)
    }

    pub fn search(&self, query: &str) -> Vec<(&str, &RegistrySkillEntry)> {
        let q = query.to_lowercase();
        let mut results: Vec<_> = self
            .skills
            .iter()
            .filter(|(name, e)| {
                name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .map(|(n, e)| (n.as_str(), e))
            .collect();
        results.sort_by_key(|k| std::cmp::Reverse(k.1.install_count));
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
schema_version: 1
updated_at: 2026-05-25T00:00:00Z
skills:
  research-prices:
    latest: 1.1.0
    description: Search and compare product prices
    publisher: human:david
    category: workflow
    tags: [e-commerce, price]
    content_sha256: "abcd"
    install_count: 42
  web-browsing:
    latest: 2.0.0
    description: Browse web pages
    publisher: human:david
    category: workflow
    tags: [web, browser]
    content_sha256: "1234"
    install_count: 128
"#;

    #[test]
    fn parses_index() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert_eq!(idx.skills.len(), 2);
        assert_eq!(idx.skills["research-prices"].latest, "1.1.0");
    }

    #[test]
    fn search_finds_by_name() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert_eq!(idx.search("price").len(), 1);
    }

    #[test]
    fn search_finds_by_tag() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert_eq!(idx.search("browser").len(), 1);
    }

    #[test]
    fn search_orders_by_install_count() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        let r = idx.search("a");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "web-browsing");
    }

    #[test]
    fn empty_query_returns_all() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert_eq!(idx.search("").len(), 2);
    }

    #[test]
    fn no_match_returns_empty() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert!(idx.search("zzz").is_empty());
    }

    #[test]
    fn recommended_roles_parses_and_defaults_empty() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert!(idx.skills["research-prices"].recommended_roles.is_empty());

        let with_roles = RegistryIndex::from_yaml(
            r#"
skills:
  writing-plans:
    latest: 1.0.0
    description: d
    publisher: mur-official
    category: workflow
    recommended_roles: [pm]
"#,
        )
        .unwrap();
        assert_eq!(
            with_roles.skills["writing-plans"].recommended_roles,
            vec!["pm".to_string()]
        );
    }

    #[test]
    fn roundtrip_yaml() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        let yaml = idx.to_yaml().unwrap();
        let idx2 = RegistryIndex::from_yaml(&yaml).unwrap();
        assert_eq!(idx.skills.len(), idx2.skills.len());
    }
}
