//! Map a role's risk level to a least-privilege entitlement plan.
use crate::agent_wizard::draft::{EntitlementPlan, RiskLevel, RoleSpec};

/// Build a least-privilege plan. `workspace` is the path the agent may read (and,
/// for higher risk, write). Sensitive paths are always denied; bash is always allowed
/// (the agent still has its own HITL gates for irreversible actions).
pub fn preset_for(role: &RoleSpec, workspace: &str) -> EntitlementPlan {
    let mut p = EntitlementPlan {
        allow_read: vec![workspace.to_string()],
        deny_path: vec!["~/.ssh".into(), "~/.aws".into(), "~/.gnupg".into()],
        tool_allow: vec!["bash".into()],
        allow_host: vec!["127.0.0.1".into(), "localhost".into()],
        ..Default::default()
    };
    match role.risk {
        RiskLevel::Low => {}
        RiskLevel::Medium => {
            p.allow_write.push(workspace.to_string());
            p.allow_spawn.extend(["git".into()]);
        }
        RiskLevel::High => {
            p.allow_write.push(workspace.to_string());
            p.allow_spawn.extend(["git".into()]);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    fn role(risk: RiskLevel) -> RoleSpec {
        RoleSpec {
            name: "x".into(),
            display_name: "X".into(),
            charter: "c".into(),
            risk,
            preset_id: None,
        }
    }

    #[test]
    fn all_presets_deny_sensitive_paths() {
        for r in [RiskLevel::Low, RiskLevel::Medium, RiskLevel::High] {
            let p = preset_for(&role(r), "/repo");
            assert!(p.deny_path.iter().any(|d| d.contains(".ssh")));
            assert!(p.tool_allow.contains(&"bash".to_string()));
        }
    }

    #[test]
    fn low_risk_has_no_write_by_default() {
        let p = preset_for(&role(RiskLevel::Low), "/repo");
        assert!(
            p.allow_write.is_empty(),
            "low-risk agents are read-only by default"
        );
    }

    #[test]
    fn high_risk_allows_repo_write_and_git() {
        let p = preset_for(&role(RiskLevel::High), "/repo");
        assert!(p.allow_write.iter().any(|w| w == "/repo"));
        assert!(p.allow_spawn.contains(&"git".to_string()));
    }
}
