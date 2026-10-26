//! `mur agent doctor` lines for profile hot reload.
//!
//! A running agent records what it did with its last `profile.yaml` /
//! global limits edit in `agent_home/reload_state.json`. Pure functions here so
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
    /// The file changed but the runtime has not re-read it yet (no reload,
    /// no error since boot); it applies on the next turn or reconcile tick.
    NotYetApplied,
    /// The newest `profile.yaml` failed to load; the agent keeps serving the
    /// last good version (`generation` successful reloads since boot). Wins
    /// over `ColdPending`: restarting onto a broken profile would not boot.
    Rejected { generation: u64 },
    /// The profile loaded, but these MCP servers' edited entries were refused
    /// at reload (pin mismatch, exec not allowed, ...); each keeps serving its
    /// last admitted entry. The reasons are in `error_lines`.
    McpRefused(Vec<String>),
    /// Some edited fields are locked until restart.
    ColdPending(Vec<String>),
}

pub fn read_state(agent_home: &Path) -> Option<ReloadState> {
    ReloadState::load(&agent_home.join(RELOAD_STATE_FILE))
}

pub fn drift_verdict(state: Option<&ReloadState>) -> DriftVerdict {
    match state {
        None => DriftVerdict::NeedsRestart,
        Some(s) if s.last_reload_error.is_some() => DriftVerdict::Rejected {
            generation: s.generation,
        },
        Some(s) if !s.mcp_refusals.is_empty() => {
            DriftVerdict::McpRefused(s.mcp_refusals.keys().cloned().collect())
        }
        Some(s) if !s.pending_cold_restart.is_empty() => {
            DriftVerdict::ColdPending(s.pending_cold_restart.clone())
        }
        Some(s) if s.generation == 0 => DriftVerdict::NotYetApplied,
        Some(s) => DriftVerdict::Reloaded {
            generation: s.generation,
        },
    }
}

/// The headline `doctor` prints for a running agent whose profile drifted.
pub fn drift_line(name: &str, verdict: &DriftVerdict) -> String {
    match verdict {
        DriftVerdict::NeedsRestart => format!(
            "{name}: running, binary current but PROFILE EDITED SINCE START \u{2192} run 'mur agent restart {name}'"
        ),
        DriftVerdict::Reloaded { generation } => {
            format!(
                "{name}: running, current (profile edits hot-reloaded, generation {generation})"
            )
        }
        DriftVerdict::NotYetApplied => format!(
            "{name}: running, profile edit not picked up yet (applies on the next turn or reconcile tick)"
        ),
        DriftVerdict::Rejected { generation: 0 } => format!(
            "{name}: running, latest profile edit REJECTED; keeping previous settings (no profile edit applied since start) \u{2192} fix profile.yaml"
        ),
        DriftVerdict::Rejected { generation } => format!(
            "{name}: running, latest profile edit REJECTED; keeping previous settings (last good reload: generation {generation}) \u{2192} fix profile.yaml"
        ),
        DriftVerdict::McpRefused(servers) => format!(
            "{name}: running, edits to MCP server(s) {} REFUSED; each keeps its previous entry \u{2192} fix profile.yaml",
            servers.join(", ")
        ),
        DriftVerdict::ColdPending(fields) => format!(
            "{name}: running, profile edits to {} need a restart \u{2192} run 'mur agent restart {name}'",
            fields.join(", ")
        ),
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
    for (server, why) in &s.mcp_refusals {
        out.push(format!(
            "  MCP server '{server}' edit REFUSED, still running its previous entry: {why}"
        ));
    }
    if let Some(e) = &s.config_limits_error {
        out.push(format!(
            "  global limits edit REJECTED, still using the last good value: {e}"
        ));
    }
    out
}

/// Host-level note on where the global limits scope is read from (#1794).
/// Empty when there is nothing to say: no legacy block, or only the new file.
pub fn global_limits_lines(mur_home: &Path) -> Vec<String> {
    use mur_common::limits::{
        GlobalSource, global_limits_path, legacy_block_present, legacy_config_path, load_global,
    };
    let new = global_limits_path(mur_home);
    let legacy = legacy_config_path(mur_home);
    match load_global(mur_home) {
        Err(e) => vec![format!(
            "global limits: {} does not load ({e}) \u{2192} fix it; agents keep their last good value",
            new.display()
        )],
        Ok(_) if !legacy_block_present(mur_home) => Vec::new(),
        Ok(g) if g.source == GlobalSource::File => vec![format!(
            "global limits: {} is in effect; the limits: block in {} is ignored (delete it by hand if you like)",
            new.display(),
            legacy.display()
        )],
        Ok(_) => vec![format!(
            "global limits: read from the legacy limits: block in {}; on Linux a running agent cannot see edits to it \u{2192} run 'mur limits --global' once to move it to {}",
            legacy.display(),
            new.display()
        )],
    }
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
    fn rejected_only_edit_is_not_reported_as_hot_reloaded() {
        // The only edit since start was invalid (`deadline: soon`): nothing is
        // live, so the headline must not claim a successful hot reload.
        let s = ReloadState {
            generation: 0,
            last_reload_error: Some("limits.deadline: invalid duration 'soon'".into()),
            ..Default::default()
        };
        let v = drift_verdict(Some(&s));
        assert_eq!(v, DriftVerdict::Rejected { generation: 0 });
        let line = drift_line("hotprobe", &v);
        assert!(
            !line.contains("hot-reloaded"),
            "misleading headline: {line}"
        );
        assert!(
            line.contains("REJECTED") && line.contains("previous settings"),
            "{line}"
        );
        let errs = error_lines(Some(&s));
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("REJECTED") && errs[0].contains("soon"));
    }

    #[test]
    fn rejection_after_good_reloads_says_latest_edit_failed() {
        let s = ReloadState {
            generation: 3,
            last_reload_error: Some("bad yaml".into()),
            ..Default::default()
        };
        let v = drift_verdict(Some(&s));
        assert_eq!(v, DriftVerdict::Rejected { generation: 3 });
        let line = drift_line("hotprobe", &v);
        assert!(
            line.contains("REJECTED") && line.contains("generation 3"),
            "{line}"
        );
    }

    #[test]
    fn config_limits_rejection_alone_keeps_profile_verdict() {
        // A bad global config.yaml limit says nothing about profile.yaml;
        // its own REJECTED line carries the warning.
        let s = ReloadState {
            generation: 1,
            config_limits_error: Some("deadline: soon".into()),
            ..Default::default()
        };
        assert_eq!(
            drift_verdict(Some(&s)),
            DriftVerdict::Reloaded { generation: 1 }
        );
    }

    #[test]
    fn edit_not_yet_seen_by_runtime_is_not_reported_as_hot_reloaded() {
        // Drift with generation 0 and no error: the runtime has not looked at
        // the new file yet (next turn or reconcile tick), so nothing is live.
        let s = ReloadState::default();
        let v = drift_verdict(Some(&s));
        assert_eq!(v, DriftVerdict::NotYetApplied);
        let line = drift_line("hotprobe", &v);
        assert!(!line.contains("hot-reloaded"), "{line}");
        assert!(line.contains("not picked up yet"), "{line}");
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

    #[test]
    fn mcp_refusal_is_not_reported_as_hot_reloaded() {
        // The profile reloaded, but one MCP entry's new command failed its pin:
        // that server still runs the old entry, so "current" would be a lie.
        let mut s = ReloadState {
            generation: 2,
            pending_cold_restart: vec!["entitlements.network".into()],
            ..Default::default()
        };
        s.mcp_refusals
            .insert("probe".into(), "binary_sha256 mismatch".into());
        let v = drift_verdict(Some(&s));
        assert_eq!(v, DriftVerdict::McpRefused(vec!["probe".into()]));
        let line = drift_line("hotprobe", &v);
        assert!(!line.contains("hot-reloaded"), "{line}");
        assert!(line.contains("probe") && line.contains("REFUSED"), "{line}");
        let errs = error_lines(Some(&s));
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("'probe'") && errs[0].contains("mismatch"));
    }

    #[test]
    fn profile_rejection_outranks_mcp_refusal() {
        let mut s = ReloadState {
            generation: 1,
            last_reload_error: Some("bad yaml".into()),
            ..Default::default()
        };
        s.mcp_refusals.insert("probe".into(), "no pin".into());
        assert_eq!(
            drift_verdict(Some(&s)),
            DriftVerdict::Rejected { generation: 1 }
        );
        assert_eq!(error_lines(Some(&s)).len(), 2);
    }

    #[test]
    fn global_limits_lines_name_the_file_in_effect() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path();
        assert!(global_limits_lines(h).is_empty(), "nothing on disk");

        std::fs::write(h.join("config.yaml"), "limits:\n  deadline: 4h\n").unwrap();
        let legacy = global_limits_lines(h);
        assert!(
            legacy[0].contains("legacy") && legacy[0].contains("mur limits --global"),
            "{legacy:?}"
        );

        std::fs::create_dir_all(mur_common::limits::global_limits_dir(h)).unwrap();
        std::fs::write(mur_common::limits::global_limits_path(h), "deadline: 1h\n").unwrap();
        let both = global_limits_lines(h);
        assert!(both[0].contains("is ignored"), "{both:?}");
        assert_eq!(
            std::fs::read_to_string(h.join("config.yaml")).unwrap(),
            "limits:\n  deadline: 4h\n",
            "doctor never edits config.yaml"
        );

        std::fs::write(h.join("config.yaml"), "{}\n").unwrap();
        assert!(global_limits_lines(h).is_empty(), "new file only");

        std::fs::write(mur_common::limits::global_limits_path(h), "deadline: [\n").unwrap();
        assert!(global_limits_lines(h)[0].contains("does not load"));
    }
}
