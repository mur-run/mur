//! The `MonitorSpec` contract. Auto- and hand-created monitors normalise to
//! this one shape (spec §`MonitorSpec` 契約). `validate()` covers the rules a
//! spec can check on its own (1, 4, 5, 7, 8); adapter reachability (2, 3) is
//! the CLI's job and idempotency uniqueness (6) is the store's.

use std::str::FromStr;
use std::time::Duration;

use mur_common::secret::SecretRef;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// Action names the executor (plan-2) knows. Validation by name here so a
/// spec cannot smuggle an unknown verb through to a future executor.
pub const KNOWN_ACTIONS: &[&str] = &[
    "notify",
    "start_downstream",
    "collect_logs",
    "apply_known_remedy",
    "rerun",
    "reschedule_monitor",
];

const DEFAULT_MODE: &str = "hybrid";
const DEFAULT_MAX_REMEDIATION: u32 = 3;
const DEFAULT_STALLED_AFTER: &str = "20m";
const DEFAULT_SOFT_DEADLINE: &str = "3h";
const DEFAULT_HARD_DEADLINE: &str = "8h";
const MAX_NAME_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    GithubActions,
    MurRun,
    Codex,
    ClaudeCode,
    Custom,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceType::GithubActions => "github_actions",
            SourceType::MurRun => "mur_run",
            SourceType::Codex => "codex",
            SourceType::ClaudeCode => "claude_code",
            SourceType::Custom => "custom",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "github_actions" => Some(SourceType::GithubActions),
            "mur_run" => Some(SourceType::MurRun),
            "codex" => Some(SourceType::Codex),
            "claude_code" => Some(SourceType::ClaudeCode),
            "custom" => Some(SourceType::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub r#type: SourceType,
    pub reference: String,
    /// A `SecretRef` string (`keychain:svc/acct`, `env:NAME`, …). Never the secret.
    #[serde(default)]
    pub credential_ref: Option<String>,
}

/// Stored verbatim; evaluated by the PolicyEngine (plan-2). Adapters already
/// normalise to the five outcomes, so nothing in this plan reads these.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Outcomes {
    #[serde(default)]
    pub success: Option<String>,
    #[serde(default)]
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub r#type: String,
    #[serde(flatten)]
    pub params: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Actions {
    #[serde(default)]
    pub on_success: Vec<Action>,
    #[serde(default)]
    pub on_failure: Vec<Action>,
    #[serde(default)]
    pub on_unknown: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default = "d_mode")]
    pub mode: String,
    #[serde(default = "d_max_remediation")]
    pub max_remediation_attempts: u32,
    #[serde(default = "d_stalled")]
    pub stalled_after: String,
    #[serde(default = "d_soft")]
    pub soft_deadline: String,
    #[serde(default = "d_hard")]
    pub hard_deadline: String,
    #[serde(default = "d_true")]
    pub retain_monitoring_after_hard_deadline: bool,
}

fn d_mode() -> String {
    DEFAULT_MODE.to_string()
}
fn d_max_remediation() -> u32 {
    DEFAULT_MAX_REMEDIATION
}
fn d_stalled() -> String {
    DEFAULT_STALLED_AFTER.to_string()
}
fn d_soft() -> String {
    DEFAULT_SOFT_DEADLINE.to_string()
}
fn d_hard() -> String {
    DEFAULT_HARD_DEADLINE.to_string()
}
fn d_true() -> bool {
    true
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: d_mode(),
            max_remediation_attempts: d_max_remediation(),
            stalled_after: d_stalled(),
            soft_deadline: d_soft(),
            hard_deadline: d_hard(),
            retain_monitoring_after_hard_deadline: d_true(),
        }
    }
}

impl Policy {
    /// Parsed durations. `validate()` guarantees these parse; the fallbacks
    /// exist only so a row read back from an old store never panics.
    pub fn stalled_after(&self) -> Duration {
        parse_or(&self.stalled_after, DEFAULT_STALLED_AFTER)
    }
    pub fn soft_deadline(&self) -> Duration {
        parse_or(&self.soft_deadline, DEFAULT_SOFT_DEADLINE)
    }
    pub fn hard_deadline(&self) -> Duration {
        parse_or(&self.hard_deadline, DEFAULT_HARD_DEADLINE)
    }
}

fn parse_or(s: &str, fallback: &str) -> Duration {
    mur_common::limits::parse_duration(s)
        .or_else(|| mur_common::limits::parse_duration(fallback))
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Notifications {
    #[serde(default)]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedBy {
    pub actor: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub originating_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSpec {
    pub schema_version: u32,
    pub name: String,
    pub source: Source,
    #[serde(default)]
    pub outcomes: Outcomes,
    #[serde(default)]
    pub actions: Actions,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default)]
    pub notifications: Notifications,
    pub idempotency_key: String,
    pub created_by: CreatedBy,
}

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("schema_version {0} is not supported (this build supports {SCHEMA_VERSION})")]
    Schema(u32),
    #[error("{0} must not be empty")]
    Empty(&'static str),
    #[error("name is longer than {MAX_NAME_LEN} characters")]
    NameTooLong,
    #[error(
        "credential_ref is not a secret reference (expected env:NAME, keychain:service/account, file:PATH, or cmd:...)"
    )]
    Credential(String),
    #[error("policy durations must satisfy stalled_after < soft_deadline < hard_deadline: {0}")]
    DeadlineOrder(String),
    #[error("unknown action type `{0}`")]
    Action(String),
    #[error("created_by.reason is required when the actor is an agent")]
    MissingReason,
    #[error("yaml: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl MonitorSpec {
    pub fn from_yaml(s: &str) -> Result<Self, SpecError> {
        Ok(serde_yaml::from_str(s)?)
    }

    pub fn validate(&self) -> Result<(), SpecError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SpecError::Schema(self.schema_version));
        }
        if self.name.trim().is_empty() {
            return Err(SpecError::Empty("name"));
        }
        if self.name.len() > MAX_NAME_LEN {
            return Err(SpecError::NameTooLong);
        }
        if self.source.reference.trim().is_empty() {
            return Err(SpecError::Empty("source.reference"));
        }
        if let Some(c) = &self.source.credential_ref {
            SecretRef::from_str(c)
                .map_err(|_| SpecError::Credential("invalid secret reference format".into()))?;
        }
        let (s, m, h) = (
            mur_common::limits::parse_duration(&self.policy.stalled_after),
            mur_common::limits::parse_duration(&self.policy.soft_deadline),
            mur_common::limits::parse_duration(&self.policy.hard_deadline),
        );
        match (s, m, h) {
            (Some(s), Some(m), Some(h)) if s < m && m < h => {}
            _ => {
                return Err(SpecError::DeadlineOrder(format!(
                    "{} / {} / {}",
                    self.policy.stalled_after, self.policy.soft_deadline, self.policy.hard_deadline
                )));
            }
        }
        for a in self
            .actions
            .on_success
            .iter()
            .chain(&self.actions.on_failure)
            .chain(&self.actions.on_unknown)
        {
            if !KNOWN_ACTIONS.contains(&a.r#type.as_str()) {
                return Err(SpecError::Action(a.r#type.clone()));
            }
        }
        if self.idempotency_key.trim().is_empty() {
            return Err(SpecError::Empty("idempotency_key"));
        }
        if self.created_by.actor.trim().is_empty() {
            return Err(SpecError::Empty("created_by.actor"));
        }
        if self.created_by.actor.starts_with("agent:") && self.created_by.reason.trim().is_empty() {
            return Err(SpecError::MissingReason);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
schema_version: 1
name: wait-for-ci
source:
  type: github_actions
  reference: owner/repo/123
  credential_ref: keychain:mur/github-default
actions:
  on_failure:
    - type: collect_logs
    - type: rerun
policy:
  stalled_after: 20m
  soft_deadline: 3h
  hard_deadline: 8h
idempotency_key: ci:owner/repo:123
created_by:
  actor: agent:commander
  reason: "CI was started and returned a trackable run id"
"#;

    #[test]
    fn parses_the_spec_example() {
        let s = MonitorSpec::from_yaml(EXAMPLE).unwrap();
        assert_eq!(s.name, "wait-for-ci");
        assert_eq!(s.source.r#type, SourceType::GithubActions);
        assert_eq!(s.actions.on_failure.len(), 2);
        assert_eq!(
            s.policy.stalled_after(),
            std::time::Duration::from_secs(20 * 60)
        );
        assert!(s.policy.retain_monitoring_after_hard_deadline);
        s.validate().unwrap();
    }

    #[test]
    fn defaults_are_the_spec_defaults() {
        let p = Policy::default();
        assert_eq!(p.stalled_after(), std::time::Duration::from_secs(20 * 60));
        assert_eq!(p.soft_deadline(), std::time::Duration::from_secs(3 * 3600));
        assert_eq!(p.hard_deadline(), std::time::Duration::from_secs(8 * 3600));
        assert_eq!(p.max_remediation_attempts, 3);
        assert!(p.retain_monitoring_after_hard_deadline);
    }

    #[test]
    fn deadline_order_is_enforced() {
        let y = EXAMPLE.replace("soft_deadline: 3h", "soft_deadline: 10h");
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::DeadlineOrder(_)), "{e}");
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let y = EXAMPLE.replace("schema_version: 1", "schema_version: 2");
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::Schema(2)));
    }

    #[test]
    fn credential_ref_must_be_a_secret_ref_not_a_secret() {
        let y = EXAMPLE.replace("keychain:mur/github-default", "ghp_plaintexttoken");
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::Credential(_)), "{e}");
    }

    #[test]
    fn unknown_action_name_is_rejected() {
        let y = EXAMPLE.replace("type: rerun", "type: deploy_prod");
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::Action(ref n) if n == "deploy_prod"));
    }

    #[test]
    fn agent_created_monitor_needs_a_reason() {
        let y = EXAMPLE.replace(
            "reason: \"CI was started and returned a trackable run id\"",
            "reason: \"\"",
        );
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        assert!(matches!(e, SpecError::MissingReason));
    }

    #[test]
    fn idempotency_key_and_reference_are_required() {
        let y = EXAMPLE.replace(
            "idempotency_key: ci:owner/repo:123",
            "idempotency_key: \"\"",
        );
        assert!(matches!(
            MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err(),
            SpecError::Empty("idempotency_key")
        ));
        let y = EXAMPLE.replace("reference: owner/repo/123", "reference: \"\"");
        assert!(matches!(
            MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err(),
            SpecError::Empty("source.reference")
        ));
    }

    #[test]
    fn credential_error_does_not_leak_the_secret() {
        let secret_token = "ghp_plaintexttoken_1234567890abcdef";
        let y = EXAMPLE.replace("keychain:mur/github-default", secret_token);
        let e = MonitorSpec::from_yaml(&y).unwrap().validate().unwrap_err();
        let error_msg = e.to_string();
        assert!(
            !error_msg.contains(secret_token),
            "error message leaked the secret: {error_msg}"
        );
    }
}
