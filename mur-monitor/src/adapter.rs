//! What an adapter hands back, and the trait every source implements.
//! `observe`/`validate_reference` are read-only by construction — nothing
//! on those two can mutate the source (spec: custom adapters "預設只讀").
//! `rerun` is plan-2's one opt-in exception: it defaults to refusing (so
//! every existing adapter stays read-only with no code change) and only
//! `GithubActionsAdapter` (`mur-core`) overrides it, gated on its own
//! separate `write_credential_ref` grant — see
//! `mur-core/src/monitor/actions/rerun.rs`.

use std::collections::HashMap;
use std::time::Duration;

use crate::spec::SourceType;
use crate::state::Outcome;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub outcome: Outcome,
    /// Opaque; only a CHANGE resets the stalled timer (spec §stalled 與期限).
    pub progress_token: Option<String>,
    /// Short, human-readable, already redacted once `redacted()` has run.
    pub evidence: String,
    /// e.g. GitHub `Retry-After`. Clamped by `backoff::clamp_recommended`.
    pub recommended_poll_after: Option<Duration>,
    /// Set when `outcome == Unknown` for a monitor-side reason.
    pub adapter_error: Option<String>,
    /// Set by an adapter that knows, structurally, that the credential
    /// itself is the blocker (unresolvable ref, 401, or a non-rate-limit
    /// 403) — never inferred by sniffing `adapter_error`'s prose, which is
    /// free-form per adapter and not a contract. `cmd::monitor::add` refuses
    /// on this flag so the caller learns immediately, instead of creating a
    /// monitor that would report `unknown` forever for a fixable problem.
    pub credential_failure: bool,
}

impl Observation {
    pub fn pending(token: impl Into<String>, evidence: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Pending,
            progress_token: Some(token.into()),
            evidence: evidence.into(),
            recommended_poll_after: None,
            adapter_error: None,
            credential_failure: false,
        }
    }
    pub fn terminal(outcome: Outcome, evidence: impl Into<String>) -> Self {
        debug_assert!(outcome.is_terminal());
        Self {
            outcome,
            progress_token: None,
            evidence: evidence.into(),
            recommended_poll_after: None,
            adapter_error: None,
            credential_failure: false,
        }
    }
    pub fn unknown(error: impl Into<String>) -> Self {
        let e = error.into();
        Self {
            outcome: Outcome::Unknown,
            progress_token: None,
            evidence: e.clone(),
            recommended_poll_after: None,
            adapter_error: Some(e),
            credential_failure: false,
        }
    }
    pub fn with_poll_after(mut self, d: Duration) -> Self {
        self.recommended_poll_after = Some(d);
        self
    }
    /// Marks this `Unknown` observation as caused by the credential itself
    /// (not a transient network/server issue) — see the field doc.
    pub fn credential_failure(mut self) -> Self {
        self.credential_failure = true;
        self
    }
    /// The single redaction chokepoint before store, CLI, or (plan-2) agent.
    pub fn redacted(mut self) -> Self {
        self.evidence = mur_common::redact::redact_secrets(&self.evidence).into_owned();
        self.adapter_error = self
            .adapter_error
            .map(|e| mur_common::redact::redact_secrets(&e).into_owned());
        self
    }
}

pub trait SourceAdapter: Send + Sync {
    fn source_type(&self) -> SourceType;
    /// Shape check only — no network. `add` also runs one real `observe`.
    fn validate_reference(&self, reference: &str) -> Result<(), String>;
    /// One read-only query. Must never panic and never block unbounded.
    fn observe(&self, reference: &str, credential_ref: Option<&str>) -> Observation;
    /// Restart whatever failed. `write_credential_ref` is the monitor's
    /// separate write grant (`Source::write_credential_ref`) — never the
    /// read-only `credential_ref` a plain `observe` uses. The default
    /// refuses: only an adapter that explicitly supports a write overrides
    /// this, so adding this method could not silently make any existing
    /// adapter (including test doubles) able to mutate its source.
    fn rerun(
        &self,
        _reference: &str,
        _write_credential_ref: Option<&str>,
    ) -> Result<String, String> {
        Err(format!(
            "source type `{}` does not support rerun",
            self.source_type().as_str()
        ))
    }
}

#[derive(Default)]
pub struct AdapterRegistry {
    map: HashMap<SourceType, Box<dyn SourceAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, adapter: Box<dyn SourceAdapter>) {
        self.map.insert(adapter.source_type(), adapter);
    }
    pub fn get(&self, t: SourceType) -> Option<&dyn SourceAdapter> {
        self.map.get(&t).map(|b| b.as_ref())
    }
    pub fn types(&self) -> Vec<SourceType> {
        let mut v: Vec<_> = self.map.keys().copied().collect();
        v.sort_by_key(|t| t.as_str());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl SourceAdapter for Fake {
        fn source_type(&self) -> SourceType {
            SourceType::Custom
        }
        fn validate_reference(&self, r: &str) -> Result<(), String> {
            if r.is_empty() {
                Err("empty".into())
            } else {
                Ok(())
            }
        }
        fn observe(&self, _r: &str, _c: Option<&str>) -> Observation {
            Observation::pending("t1", "fine")
        }
    }

    #[test]
    fn registry_finds_by_source_type() {
        let mut reg = AdapterRegistry::new();
        reg.register(Box::new(Fake));
        assert!(reg.get(SourceType::Custom).is_some());
        assert!(reg.get(SourceType::MurRun).is_none());
        assert_eq!(reg.types(), vec![SourceType::Custom]);
    }

    #[test]
    fn unknown_carries_the_error_and_is_not_terminal() {
        let o = Observation::unknown("http 503");
        assert_eq!(o.outcome, Outcome::Unknown);
        assert_eq!(o.adapter_error.as_deref(), Some("http 503"));
        assert!(!o.outcome.is_terminal());
    }

    #[test]
    fn redacted_scrubs_evidence_and_error() {
        let o = Observation::unknown("token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 rejected")
            .redacted();
        assert!(
            !o.adapter_error
                .as_deref()
                .unwrap()
                .contains("ghp_ABCDEFGHIJ"),
            "{o:?}"
        );
        let o = Observation::terminal(
            Outcome::Failed,
            "Authorization: Bearer sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
        )
        .redacted();
        assert!(
            !o.evidence.contains("sk-ant-api03-abcdef"),
            "{}",
            o.evidence
        );
    }
}
