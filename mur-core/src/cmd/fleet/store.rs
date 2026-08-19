//! Fleet YAML store — load / save / list fleets under `~/.mur/fleets/<name>/fleet.yaml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::fleet::{Fleet, valid_fleet_name};

/// Directory that holds all fleet subdirectories.
pub fn fleets_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("fleets")
}

/// Directory for a single fleet.
pub fn fleet_dir(mur_home: &Path, name: &str) -> PathBuf {
    fleets_dir(mur_home).join(name)
}

/// Path to `fleet.yaml` for a single fleet.
pub fn fleet_path(mur_home: &Path, name: &str) -> PathBuf {
    fleet_dir(mur_home, name).join("fleet.yaml")
}

/// Persist a fleet to disk. Creates parent directories as needed.
///
/// C2b defense-in-depth: rejects an invalid fleet name at the store layer so
/// the raw API can never write a traversal path regardless of caller.
pub fn save_fleet(mur_home: &Path, fleet: &Fleet) -> Result<()> {
    if !valid_fleet_name(&fleet.name) {
        bail!(
            "invalid fleet name '{}': use lowercase letters, digits, '-' or '_'",
            fleet.name
        );
    }
    let dir = fleet_dir(mur_home, &fleet.name);
    std::fs::create_dir_all(&dir).with_context(|| format!("create fleet dir {}", dir.display()))?;
    let path = fleet_path(mur_home, &fleet.name);
    let yaml = serde_yaml::to_string(fleet).context("serialize fleet")?;
    // Atomic write: write to temp then rename.
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Load a fleet from disk. Errors if the fleet does not exist.
pub fn load_fleet(mur_home: &Path, name: &str) -> Result<Fleet> {
    if !valid_fleet_name(name) {
        bail!("invalid fleet name '{name}': use lowercase letters, digits, '-' or '_'");
    }
    let path = fleet_path(mur_home, name);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("fleet '{name}' not found at {}", path.display()))?;
    let fleet: Fleet =
        serde_yaml::from_str(&raw).with_context(|| format!("parse fleet '{name}'"))?;
    // Reject a policy that grants more than config is allowed to grant, at
    // the moment a run (or daemon tick) would trust it — a fleet that sits on
    // disk with an invalid grant must not stay silent until the gate quietly
    // ignores it.
    fleet
        .hitl
        .as_ref()
        .map(|h| h.validate())
        .transpose()
        .map_err(anyhow::Error::msg)?;
    Ok(fleet)
}

/// List the names of all persisted fleets (sorted).
pub fn list_fleets(mur_home: &Path) -> Result<Vec<String>> {
    let dir = fleets_dir(mur_home);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    for entry in
        std::fs::read_dir(&dir).with_context(|| format!("read fleets dir {}", dir.display()))?
    {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            let candidate = fleet_path(mur_home, entry.file_name().to_string_lossy().as_ref());
            if candidate.exists() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;

    /// C2b — save_fleet must refuse an invalid (traversal) fleet name at the store layer.
    #[test]
    fn save_fleet_refuses_traversal_name() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let bad = Fleet {
            name: "../../evil".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec![],
            channel_id: "fleet-evil".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            team_id: None,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
        };
        let err = save_fleet(home, &bad).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid fleet name"),
            "expected traversal refusal, got: {err:#}"
        );
        // Nothing must be written outside the home.
        assert!(
            !home.join("..").join("evil").exists(),
            "traversal path must not be created"
        );
    }

    #[test]
    fn save_load_list_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let f = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            channel_id: "fleet-dev".into(),
            team_id: None,
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
        };
        save_fleet(home, &f).unwrap();
        assert!(fleet_path(home, "dev").exists());
        assert_eq!(load_fleet(home, "dev").unwrap(), f);
        assert_eq!(list_fleets(home).unwrap(), vec!["dev".to_string()]);
        assert!(load_fleet(home, "missing").is_err());
    }
}
