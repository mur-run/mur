//! Saved-workflows list for the Hub's Workflows library page.
//!
//! Reads every workflow YAML under `~/.mur/workflows/*.yaml` (source of
//! truth, see `mur_common::workflow::Workflow`). No discover/install UI here:
//! workflows arrive in the directory automatically (relay-installed or
//! authored locally); server-side discovery of a shared registry is a later
//! concern.
//!
//! Fail-open on every axis: a file that fails to parse is skipped (+
//! `tracing::warn`); a missing directory yields an empty list rather than a
//! command error the UI must handle.

use std::path::Path;

use mur_common::workflow::Workflow;
use serde::Serialize;

/// One saved workflow as shown in the Workflows library list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowView {
    pub name: String,
    pub description: String,
    pub path: String,
}

/// List every workflow under `~/.mur/workflows/*.yaml`.
/// Fail-open: any error surfaces as an empty list plus a `tracing::warn`.
#[tauri::command]
pub fn workflows_list() -> Result<Vec<WorkflowView>, String> {
    let mur_home = crate::mur_home_path();
    let workflows_dir = mur_home.join("workflows");
    Ok(list_workflows(&workflows_dir))
}

fn list_workflows(workflows_dir: &Path) -> Vec<WorkflowView> {
    let entries = match std::fs::read_dir(workflows_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %workflows_dir.display(), error = %e, "workflows_list: cannot read workflows dir");
            return vec![];
        }
    };

    let mut views = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_yaml_ng::from_str::<Workflow>(&content) {
                Ok(wf) => views.push(WorkflowView {
                    name: wf.base.name.clone(),
                    description: wf.base.description.clone(),
                    path: path.display().to_string(),
                }),
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "workflows_list: skipping unparsable workflow");
                }
            },
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "workflows_list: cannot read workflow file");
            }
        }
    }
    views.sort_by(|a, b| a.name.cmp(&b.name));
    views
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_workflows_missing_dir_returns_empty() {
        let tmp = std::env::temp_dir().join(format!(
            "mur-workflows-list-test-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let result = list_workflows(&tmp);
        assert!(result.is_empty());
    }

    #[test]
    fn list_workflows_skips_invalid_keeps_valid() {
        let tmp =
            std::env::temp_dir().join(format!("mur-workflows-list-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        std::fs::write(
            tmp.join("good.yaml"),
            r#"
name: deploy-prod
description: Deploy to production
content:
  technical: "deploys the app"
steps: []
"#,
        )
        .unwrap();

        std::fs::write(tmp.join("bad.yaml"), "not: [valid: yaml: at all").unwrap();

        let result = list_workflows(&tmp);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "deploy-prod");
        assert_eq!(result[0].description, "Deploy to production");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
