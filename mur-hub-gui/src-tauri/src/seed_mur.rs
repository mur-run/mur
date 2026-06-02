//! Seed the built-in "Mur" agent from the bundled template on first launch.
//!
//! Idempotent: seeds only when NO agents exist under `<mur_home>/agents`, so it
//! never clobbers a user who already has agents or deleted Mur on purpose.

use std::path::Path;

/// True if any agent directory already exists under `<mur_home>/agents`.
pub fn any_agent_exists(mur_home: &Path) -> bool {
    let agents = mur_home.join("agents");
    match std::fs::read_dir(&agents) {
        Ok(mut entries) => entries.any(|e| e.map(|e| e.path().is_dir()).unwrap_or(false)),
        Err(_) => false,
    }
}

/// Recursively copy `src` into `dst`.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Seed Mur from `template_dir` into `<mur_home>/agents/mur` iff no agents
/// exist. Returns Ok(true) if seeding happened, Ok(false) if skipped.
pub fn seed_if_empty(template_dir: &Path, mur_home: &Path) -> std::io::Result<bool> {
    if any_agent_exists(mur_home) {
        return Ok(false);
    }
    let dst = mur_home.join("agents").join("mur");
    copy_tree(template_dir, &dst)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_template(dir: &Path) {
        std::fs::create_dir_all(dir.join("skills")).unwrap();
        std::fs::write(dir.join("profile.yaml"), "name: Mur\n").unwrap();
        std::fs::write(dir.join("sys_prompt.md"), "# Mur\n").unwrap();
        std::fs::write(dir.join("skills/concierge.yaml"), "name: concierge\n").unwrap();
    }

    #[test]
    fn seeds_when_empty() {
        let home = TempDir::new().unwrap();
        let tpl = TempDir::new().unwrap();
        make_template(tpl.path());
        assert!(seed_if_empty(tpl.path(), home.path()).unwrap());
        assert!(home.path().join("agents/mur/profile.yaml").exists());
        assert!(home.path().join("agents/mur/skills/concierge.yaml").exists());
    }

    #[test]
    fn skips_when_an_agent_exists() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("agents/other")).unwrap();
        let tpl = TempDir::new().unwrap();
        make_template(tpl.path());
        assert!(!seed_if_empty(tpl.path(), home.path()).unwrap());
        assert!(!home.path().join("agents/mur").exists());
    }

    #[test]
    fn bundled_profile_deserializes() {
        // The real template lives next to the crate; ensure it parses as a profile.
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/resources/mur-agent-template/profile.yaml"
        );
        let body = std::fs::read_to_string(p).unwrap();
        let _profile: mur_common::AgentProfile =
            serde_yaml_ng::from_str(&body).expect("seed profile must deserialize");
    }
}
