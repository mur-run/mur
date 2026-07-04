//! `mur agent skill install-pack <role>` — installs every registry skill
//! tagged `recommended_roles: [<role>]` onto an agent, skipping ones already
//! installed. Thin batch wrapper over `skill_registry_add::cmd_skill_registry_add`.

use anyhow::Result;

use super::skill_registry_add::cmd_skill_registry_add;
use crate::cmd::skill_registry;
use mur_common::skill::registry::RegistryIndex;
use mur_common::skill::store::agent_skill_dir;

/// Registry skill names whose `recommended_roles` contains `role`, sorted.
pub fn pack_members(idx: &RegistryIndex, role: &str) -> Vec<String> {
    let mut names: Vec<String> = idx
        .skills
        .iter()
        .filter(|(_, e)| e.recommended_roles.iter().any(|r| r == role))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

/// Installs `pack_members(role)` onto `agent`, skipping already-installed
/// skills. Returns `(installed, skipped)` names.
pub async fn cmd_skill_install_pack(
    agent: &str,
    role: &str,
    yes: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let mur_home = super::resolve_mur_home()?;
    let (_dir, idx) = skill_registry::fetch_and_load(&mur_home, skill_registry::DEFAULT_REGISTRY)?;
    let members = pack_members(&idx, role);

    let mut installed = Vec::new();
    let mut skipped = Vec::new();
    for name in members {
        if agent_skill_dir(&mur_home, agent).join(&name).exists() {
            skipped.push(name);
            continue;
        }
        cmd_skill_registry_add(agent, &name, None, yes).await?;
        installed.push(name);
    }
    Ok((installed, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::registry::RegistrySkillEntry;

    fn entry(roles: &[&str]) -> RegistrySkillEntry {
        RegistrySkillEntry {
            latest: "1.0.0".into(),
            description: "d".into(),
            publisher: "mur-official".into(),
            category: "workflow".into(),
            tags: vec![],
            content_sha256: String::new(),
            install_count: 0,
            recommended_roles: roles.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn pack_members_filters_and_sorts_by_role() {
        let mut idx = RegistryIndex {
            schema_version: 1,
            updated_at: String::new(),
            skills: Default::default(),
        };
        idx.skills.insert("writing-plans".into(), entry(&["pm"]));
        idx.skills
            .insert("test-driven-development".into(), entry(&["coder"]));
        idx.skills
            .insert("systematic-debugging".into(), entry(&["coder"]));

        let mut coder = pack_members(&idx, "coder");
        coder.sort();
        assert_eq!(
            coder,
            vec![
                "systematic-debugging".to_string(),
                "test-driven-development".to_string()
            ]
        );
    }
}
