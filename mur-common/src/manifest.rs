//! AgentManifest — declarative K8s-style spec for creating/updating an agent.
//!
//! Used by both `mur agent apply` (local) and `mur commander apply` (remote).
//! Schema: `mur.run/v1 / AgentManifest`. Applied via `mur agent apply -f manifest.yaml`.

use serde::{Deserialize, Serialize};

use crate::agent::{PatternFilter, SnapshotPolicy};

pub const MANIFEST_API_VERSION: &str = "mur.run/v1";
pub const MANIFEST_KIND_AGENT: &str = "AgentManifest";

/// Top-level declarative agent manifest (K8s-style).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentManifest {
    /// Must be `"mur.run/v1"`.
    pub api_version: String,
    /// Must be `"AgentManifest"`.
    pub kind: String,
    pub metadata: ManifestMetadata,
    pub spec: ManifestSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManifestMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sys_prompt_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default)]
    pub patterns: ManifestPatterns,
    #[serde(default)]
    pub resources: ManifestResources,
    #[serde(default)]
    pub entitlements: ManifestEntitlements,
    #[serde(default)]
    pub federation: ManifestFederation,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManifestPatterns {
    #[serde(default)]
    pub filter: PatternFilter,
    #[serde(default)]
    pub snapshot_policy: SnapshotPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManifestResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget_per_day_usd: Option<f64>,
    #[serde(default)]
    pub max_concurrent_sessions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntitlements {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filesystem_read: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filesystem_write: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFederation {
    #[serde(default)]
    pub sync_interval_minutes: u32,
}

impl AgentManifest {
    /// Parse from YAML string. Returns an error if `apiVersion` or `kind` is wrong.
    pub fn from_yaml(yaml: &str) -> anyhow::Result<Self> {
        let m: AgentManifest = serde_yaml_ng::from_str(yaml)?;
        anyhow::ensure!(
            m.api_version == MANIFEST_API_VERSION,
            "expected apiVersion '{}', got '{}'",
            MANIFEST_API_VERSION,
            m.api_version
        );
        anyhow::ensure!(
            m.kind == MANIFEST_KIND_AGENT,
            "expected kind '{}', got '{}'",
            MANIFEST_KIND_AGENT,
            m.kind
        );
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
apiVersion: mur.run/v1
kind: AgentManifest
metadata:
  name: code-reviewer
  workspace: default
spec:
  sysPromptRef: sys_prompt.md
  skills:
    - skills/review-rust.md
  patterns:
    filter:
      tier: [project, core]
      importanceMin: 0.5
    snapshotPolicy: pull-on-start
  resources:
    tokenBudgetPerDayUsd: 5.0
    maxConcurrentSessions: 3
  entitlements:
    network: [github.com]
    filesystemRead: [/Users/david/Projects]
  federation:
    syncIntervalMinutes: 15
"#;

    #[test]
    fn parse_example_manifest() {
        let m = AgentManifest::from_yaml(EXAMPLE).unwrap();
        assert_eq!(m.metadata.name, "code-reviewer");
        assert_eq!(m.spec.skills.len(), 1);
        assert!((m.spec.resources.token_budget_per_day_usd.unwrap() - 5.0).abs() < f64::EPSILON);
        assert_eq!(m.spec.federation.sync_interval_minutes, 15);
    }

    #[test]
    fn rejects_wrong_api_version() {
        let bad = EXAMPLE.replace("mur.run/v1", "mur.run/v2");
        assert!(AgentManifest::from_yaml(&bad).is_err());
    }

    #[test]
    fn rejects_wrong_kind() {
        let bad = EXAMPLE.replace("AgentManifest", "WorkflowManifest");
        assert!(AgentManifest::from_yaml(&bad).is_err());
    }
}
