//! Role catalog: shipped defaults + user manifests under ~/.mur/wizard/roles/.
use crate::agent_wizard::draft::RiskLevel;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleManifest {
    pub id: String,
    pub display_name: String,
    pub charter: String,
    #[serde(default)]
    pub risk: RiskLevel,
    /// Skill topic titles the research/author stages will produce (used as stubs in --no-llm).
    #[serde(default)]
    pub skill_topics: Vec<String>,
    #[serde(default)]
    pub category: String,
}

/// Merge shipped defaults with user manifests; user id wins (override, no duplicates).
pub fn merge_catalog(shipped: Vec<RoleManifest>, user: Vec<RoleManifest>) -> Vec<RoleManifest> {
    let mut out: Vec<RoleManifest> = shipped;
    for u in user {
        if let Some(slot) = out.iter_mut().find(|m| m.id == u.id) {
            *slot = u;
        } else {
            out.push(u);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn read_dir_manifests(dir: &Path) -> Vec<RoleManifest> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x == "yaml" || x == "yml")
        })
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|s| serde_yaml_ng::from_str::<RoleManifest>(&s).ok())
        .collect()
}

/// Load shipped manifests (embedded) + user manifests from `~/.mur/wizard/roles/`.
pub fn load_catalog(mur_home: &Path) -> Vec<RoleManifest> {
    let shipped = shipped_manifests();
    let user = read_dir_manifests(&mur_home.join("wizard").join("roles"));
    merge_catalog(shipped, user)
}

/// Shipped defaults are embedded at compile time so the binary is self-contained.
fn shipped_manifests() -> Vec<RoleManifest> {
    const FILES: &[&str] = &[
        include_str!("../../resources/wizard-roles/pm.yaml"),
        include_str!("../../resources/wizard-roles/qa.yaml"),
        include_str!("../../resources/wizard-roles/repomanager.yaml"),
        include_str!("../../resources/wizard-roles/rustsmith.yaml"),
        include_str!("../../resources/wizard-roles/devops.yaml"),
        include_str!("../../resources/wizard-roles/secreviewer.yaml"),
        include_str!("../../resources/wizard-roles/techwriter.yaml"),
        include_str!("../../resources/wizard-roles/dataml.yaml"),
        include_str!("../../resources/wizard-roles/frontend.yaml"),
        include_str!("../../resources/wizard-roles/support.yaml"),
    ];
    FILES
        .iter()
        .filter_map(|s| serde_yaml_ng::from_str(s).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_a_manifest_from_yaml_str() {
        let y = r#"
id: pm
display_name: PM
charter: Turns intent into buildable work.
risk: low
category: product
skill_topics: ["product-spec-and-prd-writing", "issue-authoring-and-hygiene"]
"#;
        let m: RoleManifest = serde_yaml_ng::from_str(y).unwrap();
        assert_eq!(m.id, "pm");
        assert_eq!(m.skill_topics.len(), 2);
        assert_eq!(m.risk, RiskLevel::Low);
    }

    #[test]
    fn all_shipped_manifests_parse_and_are_well_formed() {
        // filter_map silently drops bad YAML, so assert the FULL count parses + is well-formed.
        let shipped = shipped_manifests();
        assert_eq!(shipped.len(), 10, "every shipped role manifest must parse");
        for m in &shipped {
            assert!(!m.id.is_empty(), "role missing id");
            assert!(!m.display_name.is_empty(), "{} missing display_name", m.id);
            assert!(!m.skill_topics.is_empty(), "{} has no skill_topics", m.id);
        }
        // The Plan-5 additions are present.
        for id in [
            "devops",
            "secreviewer",
            "techwriter",
            "dataml",
            "frontend",
            "support",
        ] {
            assert!(
                shipped.iter().any(|m| m.id == id),
                "missing shipped role {id}"
            );
        }
    }

    #[test]
    fn user_dir_overrides_shipped_by_id() {
        let shipped = vec![RoleManifest {
            id: "pm".into(),
            display_name: "PM".into(),
            charter: "a".into(),
            risk: RiskLevel::Low,
            skill_topics: vec![],
            category: "product".into(),
        }];
        let user = vec![RoleManifest {
            id: "pm".into(),
            display_name: "PM (mine)".into(),
            charter: "b".into(),
            risk: RiskLevel::Low,
            skill_topics: vec![],
            category: "product".into(),
        }];
        let merged = merge_catalog(shipped, user);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].display_name, "PM (mine)");
    }
}
