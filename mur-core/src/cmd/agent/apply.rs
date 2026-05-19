use anyhow::{Context, Result, bail};
use mur_common::AgentManifest;
use std::path::Path;

use super::{load_profile_for_edit, resolve_mur_home, save_profile};

/// `mur agent apply -f manifest.yaml` — create or update an agent from a manifest.
pub fn cmd_agent_apply(file: &Path) -> Result<()> {
    let yaml = std::fs::read_to_string(file)
        .with_context(|| format!("read manifest {}", file.display()))?;
    let manifest = AgentManifest::from_yaml(&yaml)
        .with_context(|| format!("parse manifest {}", file.display()))?;

    let name = &manifest.metadata.name;
    if name.is_empty() {
        bail!("manifest metadata.name is required");
    }

    let agent_home = resolve_mur_home()?.join("agents").join(name);
    let created = !agent_home.exists();

    if created {
        super::lifecycle::cmd_create(name, true, None, None, None)?;
    }

    // Apply federation config from the manifest.
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.federation.filter = manifest.spec.patterns.filter.clone();
    profile.federation.filter.snapshot_policy = manifest.spec.patterns.snapshot_policy.clone();
    if manifest.spec.federation.sync_interval_minutes > 0 {
        profile.federation.evidence_flush_interval_minutes =
            manifest.spec.federation.sync_interval_minutes;
    }
    save_profile(&path, &mut profile)?;

    let verb = if created { "created" } else { "updated" };
    println!("Agent '{name}' {verb} from manifest {}", file.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const MANIFEST: &str = r#"
apiVersion: mur.run/v1
kind: AgentManifest
metadata:
  name: ""
spec:
  patterns:
    filter:
      tier: [core]
      importanceMin: 0.5
"#;

    #[test]
    fn rejects_empty_metadata_name() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("m.yaml");
        std::fs::write(&p, MANIFEST).unwrap();
        let err = cmd_agent_apply(&p).unwrap_err();
        assert!(err.to_string().contains("metadata.name"));
    }

    #[test]
    fn rejects_missing_file() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("nonexistent.yaml");
        assert!(cmd_agent_apply(&p).is_err());
    }
}
