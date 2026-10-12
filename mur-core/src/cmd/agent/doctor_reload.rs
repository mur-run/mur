//! `mur agent doctor` lines for profile hot reload.
//!
//! A running agent records what it did with its last `profile.yaml` /
//! `config.yaml` edit in `agent_home/reload_state.json`. Pure functions here so
//! the wording is unit-tested without a live agent.

use mur_common::agent::{RELOAD_STATE_FILE, ReloadState};
use std::path::Path;

/// What `doctor` should say about an agent whose profile changed since start.
#[derive(Debug, PartialEq, Eq)]
pub enum DriftVerdict {
    /// Runtime predates hot reload (no state file): only a restart applies edits.
    NeedsRestart,
    /// Every edit was hot and is already live — no action.
    Reloaded { generation: u64 },
    /// Some edited fields are locked until restart.
    ColdPending(Vec<String>),
}

pub fn read_state(agent_home: &Path) -> Option<ReloadState> {
    ReloadState::load(&agent_home.join(RELOAD_STATE_FILE))
}

pub fn drift_verdict(state: Option<&ReloadState>) -> DriftVerdict {
    match state {
        None => DriftVerdict::NeedsRestart,
        Some(s) if !s.pending_cold_restart.is_empty() => {
            DriftVerdict::ColdPending(s.pending_cold_restart.clone())
        }
        Some(s) => DriftVerdict::Reloaded {
            generation: s.generation,
        },
    }
}

/// Indented warning lines for rejected edits; empty when both files loaded.
pub fn error_lines(state: Option<&ReloadState>) -> Vec<String> {
    let Some(s) = state else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(e) = &s.last_reload_error {
        out.push(format!(
            "  profile.yaml edit REJECTED, still running the last good version: {e}"
        ));
    }
    if let Some(e) = &s.config_limits_error {
        out.push(format!(
            "  config.yaml limits edit REJECTED, still using the last good value: {e}"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_state_file_means_an_old_runtime_that_needs_restart() {
        assert_eq!(drift_verdict(None), DriftVerdict::NeedsRestart);
        assert!(error_lines(None).is_empty());
    }

    #[test]
    fn hot_only_edit_is_reported_as_live() {
        let s = ReloadState {
            generation: 2,
            ..Default::default()
        };
        assert_eq!(
            drift_verdict(Some(&s)),
            DriftVerdict::Reloaded { generation: 2 }
        );
    }

    #[test]
    fn locked_field_edit_still_asks_for_restart() {
        let s = ReloadState {
            generation: 1,
            pending_cold_restart: vec!["entitlements.network".into()],
            ..Default::default()
        };
        assert_eq!(
            drift_verdict(Some(&s)),
            DriftVerdict::ColdPending(vec!["entitlements.network".into()])
        );
    }

    #[test]
    fn both_rejections_are_listed() {
        let s = ReloadState {
            last_reload_error: Some("bad yaml".into()),
            config_limits_error: Some("deadline: soon".into()),
            ..Default::default()
        };
        let lines = error_lines(Some(&s));
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("bad yaml") && lines[1].contains("soon"));
    }
}
