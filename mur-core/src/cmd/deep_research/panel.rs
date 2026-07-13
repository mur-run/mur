//! Bare `mur deep-research` status panel (read-only).

use std::path::Path;

use super::status::{DEFAULT_FLEET_NAME, DeepResearchStatus, collect_status};

pub fn render_panel(s: &DeepResearchStatus) -> String {
    if s.workers.is_empty() {
        return "Deep research is not set up yet.\n  Run `mur deep-research setup` to configure workers, model, budget and egress.\n".to_string();
    }
    let mut out = String::from("Deep research status\n");
    out.push_str(&format!(
        "  model: {}\n",
        s.model.as_deref().unwrap_or("(none — run setup)")
    ));
    out.push_str(&format!(
        "  fleet: {}\n",
        if s.fleet_exists {
            DEFAULT_FLEET_NAME
        } else {
            "(missing — run setup)"
        }
    ));
    for w in &s.workers {
        out.push_str(&format!(
            "  {} — {}, egress {}\n",
            w.name,
            if w.running { "running" } else { "stopped" },
            if w.egress_granted {
                "granted"
            } else {
                "NOT granted"
            },
        ));
    }
    out.push_str("\nRun research with: mur deep-research \"<your question>\"\n");
    out
}

pub fn cmd_panel(mur_home: &Path) -> anyhow::Result<()> {
    print!(
        "{}",
        render_panel(&collect_status(mur_home, DEFAULT_FLEET_NAME))
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::deep_research::status::{DeepResearchStatus, WorkerStatus};

    #[test]
    fn panel_unconfigured_points_at_setup() {
        let s = DeepResearchStatus {
            workers: vec![],
            fleet_exists: false,
            model: None,
        };
        let out = render_panel(&s);
        assert!(out.contains("mur deep-research setup"));
    }

    #[test]
    fn panel_lists_workers_and_egress() {
        let s = DeepResearchStatus {
            workers: vec![WorkerStatus {
                name: "dr_worker_1".into(),
                running: true,
                egress_granted: true,
            }],
            fleet_exists: true,
            model: Some("claude_haiku".into()),
        };
        let out = render_panel(&s);
        assert!(out.contains("dr_worker_1"));
        assert!(out.contains("running"));
        assert!(out.contains("claude_haiku"));
    }
}
