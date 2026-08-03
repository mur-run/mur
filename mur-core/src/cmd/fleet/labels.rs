//! Label registry store — `~/.mur/labels.yaml`.
//!
//! One central file, atomically written (tmp + rename) exactly like
//! `store::save_fleet`. A missing or corrupt file is *not* an error: the Hub
//! must degrade to an unlabelled flat list rather than fail to render.
//!
//! Every function here is reached through the lib target, by `mur-hub-gui`'s
//! Tauri commands. Labels are a Hub surface with no CLI subcommand, so the
//! `mur` binary's own module tree sees them as unused and `-D warnings` fails
//! the build without this. The allow is scoped to the module rather than to
//! each function so that adding one later does not re-trip it — and it comes
//! off the day `mur fleet label` exists.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::fleet::valid_fleet_name;
use mur_common::labels::{Label, LabelRegistry, valid_label_id};

use super::store;

/// Path to the central label registry.
pub fn labels_path(mur_home: &Path) -> PathBuf {
    mur_home.join("labels.yaml")
}

/// Load the registry, self-healed. Missing or unparseable → empty registry.
pub fn load(mur_home: &Path) -> LabelRegistry {
    let Ok(raw) = std::fs::read_to_string(labels_path(mur_home)) else {
        return LabelRegistry::default();
    };
    let mut reg: LabelRegistry = serde_yaml::from_str(&raw).unwrap_or_default();
    reg.normalize();
    reg
}

/// Persist the registry atomically. Normalizes first, so no invalid id is ever
/// written even if a caller built the struct by hand.
pub fn save(mur_home: &Path, reg: &LabelRegistry) -> Result<()> {
    let mut reg = reg.clone();
    reg.normalize();
    std::fs::create_dir_all(mur_home)
        .with_context(|| format!("create mur home {}", mur_home.display()))?;
    let path = labels_path(mur_home);
    let yaml = serde_yaml::to_string(&reg).context("serialize label registry")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// Create a label. Rejects an invalid id and a duplicate.
pub fn create_label(mur_home: &Path, id: &str, display: &str, color: Option<String>) -> Result<()> {
    if !valid_label_id(id) {
        bail!("invalid label id '{id}': use lowercase letters, digits, '-' or '_' (max 32)");
    }
    let mut reg = load(mur_home);
    if reg.contains(id) {
        bail!("label '{id}' already exists");
    }
    reg.labels.push(Label {
        id: id.to_string(),
        display: if display.is_empty() {
            id.to_string()
        } else {
            display.to_string()
        },
        color,
    });
    save(mur_home, &reg)
}

/// Change a label's display text (its id stays stable — it is the key).
pub fn rename_label(mur_home: &Path, id: &str, display: &str) -> Result<()> {
    let mut reg = load(mur_home);
    if !reg.rename_label(id, display) {
        bail!("label '{id}' not found");
    }
    save(mur_home, &reg)
}

/// Delete a label and scrub it from every assignment.
pub fn delete_label(mur_home: &Path, id: &str) -> Result<()> {
    let mut reg = load(mur_home);
    if !reg.contains(id) {
        bail!("label '{id}' not found");
    }
    reg.delete_label(id);
    save(mur_home, &reg)
}

/// Replace a fleet's labels. Order matters: `ids[0]` becomes the primary label,
/// i.e. the group the fleet is listed under.
pub fn set_labels(mur_home: &Path, fleet: &str, ids: Vec<String>) -> Result<()> {
    if !valid_fleet_name(fleet) {
        bail!("invalid fleet name '{fleet}'");
    }
    let mut reg = load(mur_home);
    for id in &ids {
        if !reg.contains(id) {
            bail!("unknown label '{id}'");
        }
    }
    reg.set_labels(fleet, ids);
    save(mur_home, &reg)
}

/// Drop assignments for fleets that no longer exist on disk.
pub fn prune(mur_home: &Path) -> Result<()> {
    let existing = store::list_fleets(mur_home).unwrap_or_default();
    let mut reg = load(mur_home);
    reg.prune(&existing);
    save(mur_home, &reg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn missing_file_loads_as_empty_registry() {
        let tmp = home();
        let reg = load(tmp.path());
        assert_eq!(reg, LabelRegistry::default());
    }

    #[test]
    fn corrupt_file_degrades_instead_of_failing() {
        let tmp = home();
        std::fs::write(labels_path(tmp.path()), "this: [is: not: a: registry").unwrap();
        assert_eq!(load(tmp.path()), LabelRegistry::default());
    }

    #[test]
    fn create_set_load_roundtrip() {
        let tmp = home();
        let h = tmp.path();
        create_label(h, "web", "Web", Some("#4a9eff".into())).unwrap();
        create_label(h, "rust", "", None).unwrap();
        set_labels(h, "develop-web", vec!["web".into(), "rust".into()]).unwrap();

        let reg = load(h);
        assert_eq!(reg.labels_of("develop-web"), ["web", "rust"]);
        assert_eq!(reg.primary_of("develop-web"), Some("web"));
        assert_eq!(reg.get("rust").unwrap().display_or_id(), "rust");
        assert_eq!(reg.get("web").unwrap().color.as_deref(), Some("#4a9eff"));
    }

    #[test]
    fn create_refuses_traversal_id_and_writes_nothing() {
        let tmp = home();
        let err = create_label(tmp.path(), "../evil", "", None).unwrap_err();
        assert!(format!("{err:#}").contains("invalid label id"));
        assert!(!labels_path(tmp.path()).exists());
    }

    #[test]
    fn create_refuses_duplicate() {
        let tmp = home();
        create_label(tmp.path(), "web", "Web", None).unwrap();
        let err = create_label(tmp.path(), "web", "Web again", None).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"));
    }

    #[test]
    fn set_labels_rejects_unknown_id() {
        let tmp = home();
        create_label(tmp.path(), "web", "Web", None).unwrap();
        let err = set_labels(tmp.path(), "develop-web", vec!["ghost".into()]).unwrap_err();
        assert!(format!("{err:#}").contains("unknown label"));
    }

    #[test]
    fn set_empty_makes_fleet_ungrouped() {
        let tmp = home();
        let h = tmp.path();
        create_label(h, "web", "Web", None).unwrap();
        set_labels(h, "develop-web", vec!["web".into()]).unwrap();
        set_labels(h, "develop-web", vec![]).unwrap();
        assert_eq!(load(h).primary_of("develop-web"), None);
    }

    #[test]
    fn delete_label_scrubs_assignments_on_disk() {
        let tmp = home();
        let h = tmp.path();
        create_label(h, "web", "Web", None).unwrap();
        create_label(h, "rust", "Rust", None).unwrap();
        set_labels(h, "develop-web", vec!["web".into(), "rust".into()]).unwrap();
        delete_label(h, "web").unwrap();

        let reg = load(h);
        assert!(!reg.contains("web"));
        assert_eq!(reg.labels_of("develop-web"), ["rust"]);
    }

    #[test]
    fn unknown_ids_hand_edited_into_the_file_are_dropped_on_load() {
        let tmp = home();
        std::fs::write(
            labels_path(tmp.path()),
            "labels:\n  - id: web\n    display: Web\nassignments:\n  develop-web: [ghost, web]\n",
        )
        .unwrap();
        assert_eq!(load(tmp.path()).labels_of("develop-web"), ["web"]);
    }

    #[test]
    fn prune_drops_fleets_that_no_longer_exist() {
        let tmp = home();
        let h = tmp.path();
        // One real fleet on disk, one only in the registry.
        std::fs::create_dir_all(store::fleet_dir(h, "dev")).unwrap();
        std::fs::write(
            store::fleet_path(h, "dev"),
            "name: dev\nchannel_id: fleet-dev\n",
        )
        .unwrap();
        create_label(h, "web", "Web", None).unwrap();
        set_labels(h, "dev", vec!["web".into()]).unwrap();
        set_labels(h, "ghost-fleet", vec!["web".into()]).unwrap();

        prune(h).unwrap();
        let reg = load(h);
        assert_eq!(reg.labels_of("dev"), ["web"]);
        assert_eq!(reg.labels_of("ghost-fleet"), Vec::<String>::new());
    }
}
