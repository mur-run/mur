//! Named model registry shared by all agents.
//!
//! On disk: `~/.mur/models.yaml`. Schema:
//!
//! ```yaml
//! schema_version: 1
//! models:
//!   anthropic_opus_4_7:
//!     provider: anthropic
//!     model: claude-opus-4-7
//!     secret: env:ANTHROPIC_API_KEY
//!     capabilities: [chat, tools]
//! ```

use crate::route::{RoutePolicy, RouteTier};
use crate::secret::SecretRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelEntry {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
    /// Routing tier: cheap/local vs frontier/expensive.
    /// When absent, the router infers based on provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<RouteTier>,
    /// Estimated USD cost per 1000 output tokens.
    /// Used for ledger cost estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_tokens: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RoleEntry {
    /// Registry model ID (key in `models:`) to use as primary.
    pub primary: String,
    /// Fallback model ID if primary is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Optional daily cost cap in USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_budget_per_day_usd: Option<f64>,
    /// If true, only use local models when handling sensitive data.
    #[serde(default)]
    pub privacy_local_only: bool,
    /// Per-role routing policy override.
    /// When absent, the router uses the default heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_policy: Option<RoutePolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRegistry {
    pub schema_version: u32,
    #[serde(default)]
    pub models: BTreeMap<String, ModelEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, RoleEntry>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self {
            schema_version: 1,
            models: BTreeMap::new(),
            roles: BTreeMap::new(),
        }
    }
}

impl ModelRegistry {
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(path)?;
        if body.trim().is_empty() {
            return Ok(Self::default());
        }
        Ok(serde_yaml_ng::from_str(&body)?)
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_yaml_ng::to_string(self)?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn default_path() -> anyhow::Result<PathBuf> {
        // Honor MUR_HOME (used by test harnesses and Windows CI, where
        // `dirs::home_dir()` reads SHGetKnownFolderPath and ignores HOME).
        if let Ok(p) = std::env::var("MUR_HOME")
            && !p.is_empty()
        {
            return Ok(PathBuf::from(p).join("models.yaml"));
        }
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
        Ok(home.join(".mur/models.yaml"))
    }

    /// Return the primary model ID for `role`, or the fallback if the primary
    /// is not in the `models` map, or `None` if the role is not configured.
    pub fn resolve_role(&self, role: &str) -> Option<&str> {
        let entry = self.roles.get(role)?;
        if self.models.contains_key(&entry.primary) {
            return Some(&entry.primary);
        }
        // primary not in registry — try fallback
        if let Some(fb) = &entry.fallback
            && self.models.contains_key(fb)
        {
            return Some(fb);
        }
        // role configured but no available model
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_registry() {
        let yaml = r#"
schema_version: 1
models:
  anthropic_opus_4_7:
    provider: anthropic
    model: claude-opus-4-7
    secret: env:ANTHROPIC_API_KEY
    capabilities: [chat, tools]
  ollama_llama3:
    provider: ollama
    model: llama3.2:3b
    base_url: http://127.0.0.1:11434
"#;
        let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(r.schema_version, 1);
        assert_eq!(r.models.len(), 2);
        let opus = r.models.get("anthropic_opus_4_7").unwrap();
        assert_eq!(opus.provider, "anthropic");
        assert_eq!(
            opus.secret,
            Some(SecretRef::Env("ANTHROPIC_API_KEY".into()))
        );
        assert!(r.models["ollama_llama3"].secret.is_none());
    }

    #[test]
    fn round_trip_preserves_shape() {
        let mut r = ModelRegistry::default();
        r.models.insert(
            "foo".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                base_url: None,
                secret: Some(SecretRef::Keychain {
                    service: "mur".into(),
                    account: "anthropic".into(),
                }),
                capabilities: vec!["chat".into()],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
            },
        );
        let s = serde_yaml_ng::to_string(&r).unwrap();
        let parsed: ModelRegistry = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(r, parsed);
    }

    #[test]
    fn rejects_unknown_secret_scheme() {
        let yaml = r#"
schema_version: 1
models:
  bad:
    provider: x
    model: y
    secret: bogus:value
"#;
        let r: Result<ModelRegistry, _> = serde_yaml_ng::from_str(yaml);
        assert!(r.is_err(), "should reject unknown scheme");
    }

    #[test]
    fn test_registry_roundtrip_with_roles() {
        let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
roles:
  reflector:
    primary: haiku
    fallback: null
    cost_budget_per_day_usd: 0.5
"#;
        let reg: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(reg.roles["reflector"].primary, "haiku");
        let back = serde_yaml_ng::to_string(&reg).unwrap();
        let reg2: ModelRegistry = serde_yaml_ng::from_str(&back).unwrap();
        assert_eq!(reg, reg2);
    }

    #[test]
    fn test_resolve_role_primary() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "haiku".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
            },
        );
        reg.roles.insert(
            "reflector".into(),
            RoleEntry {
                primary: "haiku".into(),
                fallback: None,
                ..Default::default()
            },
        );
        assert_eq!(reg.resolve_role("reflector"), Some("haiku"));
    }

    #[test]
    fn test_resolve_role_fallback() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "haiku".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
            },
        );
        reg.roles.insert(
            "reflector".into(),
            RoleEntry {
                primary: "nonexistent".into(),
                fallback: Some("haiku".into()),
                ..Default::default()
            },
        );
        assert_eq!(reg.resolve_role("reflector"), Some("haiku"));
    }

    #[test]
    fn test_resolve_role_none() {
        let reg = ModelRegistry::default();
        assert_eq!(reg.resolve_role("reflector"), None);
    }

    #[test]
    fn model_entry_parses_tier_field() {
        let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
    tier: local
  opus:
    provider: anthropic
    model: claude-opus-4-7
    tier: frontier
    cost_per_1k_tokens: 0.015
"#;
        let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(r.models["haiku"].tier, Some(RouteTier::Local));
        assert_eq!(r.models["opus"].tier, Some(RouteTier::Frontier));
        assert_eq!(r.models["opus"].cost_per_1k_tokens, Some(0.015));
        // Missing tier is None.
        let mut r2 = ModelRegistry::default();
        r2.models.insert(
            "x".into(),
            ModelEntry {
                provider: "ollama".into(),
                model: "llama3".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
            },
        );
        let yaml = serde_yaml_ng::to_string(&r2).unwrap();
        assert!(
            !yaml.contains("tier:"),
            "absent tier should not be serialized: {yaml}"
        );
    }

    #[test]
    fn role_entry_parses_route_policy() {
        let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
  opus:
    provider: anthropic
    model: claude-opus-4-7
roles:
  dev:
    primary: opus
    route_policy: !force_frontier
      model_id: opus
  reflector:
    primary: haiku
    route_policy: prefer_local
  curator:
    primary: haiku
    route_policy: force_local
  chat:
    primary: haiku
"#;
        let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(
            r.roles["dev"].route_policy,
            Some(RoutePolicy::ForceFrontier {
                model_id: "opus".into()
            })
        );
        assert_eq!(
            r.roles["reflector"].route_policy,
            Some(RoutePolicy::PreferLocal)
        );
        assert_eq!(
            r.roles["curator"].route_policy,
            Some(RoutePolicy::ForceLocal)
        );
        assert_eq!(r.roles["chat"].route_policy, None);
    }
}

#[cfg(test)]
mod io_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_returns_empty_when_file_missing() {
        let dir = tempdir().unwrap();
        let r = ModelRegistry::load_from(&dir.path().join("nope.yaml")).unwrap();
        assert_eq!(r.models.len(), 0);
        assert_eq!(r.schema_version, 1);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("models.yaml");
        let mut r = ModelRegistry::default();
        r.models.insert(
            "x".into(),
            ModelEntry {
                provider: "ollama".into(),
                model: "llama3.2:3b".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
            },
        );
        r.save_to(&p).unwrap();
        let r2 = ModelRegistry::load_from(&p).unwrap();
        assert_eq!(r, r2);
    }

    #[test]
    fn save_uses_atomic_rename() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("models.yaml");
        ModelRegistry::default().save_to(&p).unwrap();
        let temp = dir.path().join("models.yaml.tmp");
        assert!(!temp.exists(), "atomic temp left behind");
    }
}
