//! Seed the built-in "Mur" agent from the bundled template on first launch.
//!
//! Idempotent: seeds only when an agent named `mur` is not already present, so it
//! never clobbers an existing Mur or a user who deleted Mur on purpose — but,
//! unlike a blanket "directory is empty" check, it still provides the built-in
//! concierge to users who already have other agents.

use std::path::Path;

/// True if Mur is already seeded, i.e. `<mur_home>/agents/mur/profile.yaml` exists.
/// We key off the profile file (not just the directory) so a previously broken
/// half-seeded `agents/mur` directory is treated as "not seeded" and gets healed.
pub fn mur_seeded(mur_home: &Path) -> bool {
    mur_home
        .join("agents")
        .join("mur")
        .join("profile.yaml")
        .is_file()
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

/// Seed Mur from `template_dir` into `<mur_home>/agents/mur` iff Mur is not already
/// seeded. Returns Ok(true) if seeding happened, Ok(false) if skipped.
///
/// The copy is staged in a sibling temp directory and renamed into place so a
/// failure part-way through never leaves a broken `agents/mur`. If a previous run
/// left an empty/broken `agents/mur` (e.g. the template could not be found), it is
/// replaced.
pub fn seed_mur_if_missing(template_dir: &Path, mur_home: &Path) -> std::io::Result<bool> {
    if mur_seeded(mur_home) {
        return Ok(false);
    }
    // Validate the template up-front so we never create a destination dir for a
    // source that does not exist.
    if !template_dir.join("profile.yaml").is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "seed template missing profile.yaml at {}",
                template_dir.display()
            ),
        ));
    }

    let agents = mur_home.join("agents");
    std::fs::create_dir_all(&agents)?;
    let staging = agents.join(".mur.seeding");
    let dst = agents.join("mur");

    // Clean any leftover staging from a previous interrupted run.
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    copy_tree(template_dir, &staging)?;

    // Replace any broken/empty existing dir, then atomically move staging into place.
    if dst.exists() {
        std::fs::remove_dir_all(&dst)?;
    }
    std::fs::rename(&staging, &dst)?;
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
    fn seeds_when_missing() {
        let home = TempDir::new().unwrap();
        let tpl = TempDir::new().unwrap();
        make_template(tpl.path());
        assert!(seed_mur_if_missing(tpl.path(), home.path()).unwrap());
        assert!(home.path().join("agents/mur/profile.yaml").exists());
        assert!(
            home.path()
                .join("agents/mur/skills/concierge.yaml")
                .exists()
        );
    }

    #[test]
    fn seeds_even_when_other_agents_exist() {
        // Existing users with other agents must still get the built-in Mur.
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("agents/other")).unwrap();
        let tpl = TempDir::new().unwrap();
        make_template(tpl.path());
        assert!(seed_mur_if_missing(tpl.path(), home.path()).unwrap());
        assert!(home.path().join("agents/mur/profile.yaml").exists());
    }

    #[test]
    fn skips_when_mur_already_seeded() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("agents/mur")).unwrap();
        std::fs::write(home.path().join("agents/mur/profile.yaml"), "name: Mur\n").unwrap();
        let tpl = TempDir::new().unwrap();
        make_template(tpl.path());
        assert!(!seed_mur_if_missing(tpl.path(), home.path()).unwrap());
    }

    #[test]
    fn heals_broken_empty_mur_dir() {
        // A previous run left an empty agents/mur (no profile.yaml) — re-seed it.
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("agents/mur")).unwrap();
        let tpl = TempDir::new().unwrap();
        make_template(tpl.path());
        assert!(seed_mur_if_missing(tpl.path(), home.path()).unwrap());
        assert!(home.path().join("agents/mur/profile.yaml").exists());
    }

    #[test]
    fn missing_template_errors_without_creating_dst() {
        let home = TempDir::new().unwrap();
        let tpl = TempDir::new().unwrap(); // empty: no profile.yaml
        assert!(seed_mur_if_missing(tpl.path(), home.path()).is_err());
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
