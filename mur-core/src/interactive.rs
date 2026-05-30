//! Enhanced CLI interactions — template system.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;

// ─── Templates ─────────────────────────────────────────────────────

/// Built-in template definitions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Template {
    Insight,
    Technique,
    Pitfall,
    Checklist,
    Custom,
}

impl Template {
    pub fn all() -> &'static [Template] {
        &[
            Template::Insight,
            Template::Technique,
            Template::Pitfall,
            Template::Checklist,
            Template::Custom,
        ]
    }

    #[allow(dead_code)] // Public API — used by template init
    pub fn file_name(&self) -> &'static str {
        match self {
            Template::Insight => "insight.yaml",
            Template::Technique => "technique.yaml",
            Template::Pitfall => "pitfall.yaml",
            Template::Checklist => "checklist.yaml",
            Template::Custom => "custom.yaml",
        }
    }

    /// Generate template content for the description field.
    #[allow(dead_code)] // Public API — used by template init
    pub fn description_hint(&self) -> &'static str {
        match self {
            Template::Insight => "Observed that...",
            Template::Technique => "How to...",
            Template::Pitfall => "Avoid...",
            Template::Checklist => "Steps to...",
            Template::Custom => "",
        }
    }

    /// Generate template content for the technical layer.
    pub fn technical_template(&self) -> &'static str {
        match self {
            Template::Insight => "Key insight: ",
            Template::Technique => "## Steps\n\n1. \n2. \n3. \n\n## Example\n\n",
            Template::Pitfall => "## Problem\n\n## Why It Happens\n\n## Correct Approach\n\n",
            Template::Checklist => "- [ ] Step 1\n- [ ] Step 2\n- [ ] Step 3\n",
            Template::Custom => "",
        }
    }
}

/// Ensure default templates exist in ~/.mur/templates/.
#[allow(dead_code)] // Public API — called from `mur init`
pub fn ensure_default_templates() -> Result<PathBuf> {
    let templates_dir = templates_dir();
    fs::create_dir_all(&templates_dir)?;

    for tpl in Template::all() {
        let path = templates_dir.join(tpl.file_name());
        if !path.exists() {
            let yaml = format!(
                "# MUR Pattern Template: {}\n# Edit this file to customize the template.\n\ndescription: \"{}\"\ntechnical: |\n  {}\n",
                tpl.file_name().trim_end_matches(".yaml"),
                tpl.description_hint(),
                tpl.technical_template().replace('\n', "\n  "),
            );
            fs::write(&path, yaml)?;
        }
    }

    Ok(templates_dir)
}

fn templates_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("templates")
}
