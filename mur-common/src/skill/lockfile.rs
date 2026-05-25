use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;
pub const FILE_NAME: &str = "skill.lock";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillLock {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub locked: BTreeMap<String, String>,
    #[serde(default)]
    pub installed_at: String,
}

fn default_schema() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, thiserror::Error)]
pub enum LockfileError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("parse: {0}")]
    Parse(#[from] serde_yaml_ng::Error),
}

impl SkillLock {
    pub fn path(skill_dir: &Path) -> std::path::PathBuf {
        skill_dir.join(FILE_NAME)
    }

    pub fn read(skill_dir: &Path) -> Result<Self, LockfileError> {
        let p = Self::path(skill_dir);
        if !p.exists() {
            return Ok(Self {
                schema_version: SCHEMA_VERSION,
                ..Default::default()
            });
        }
        let s = fs::read_to_string(&p)?;
        if s.trim().is_empty() {
            return Ok(Self {
                schema_version: SCHEMA_VERSION,
                ..Default::default()
            });
        }
        Ok(serde_yaml_ng::from_str(&s)?)
    }

    pub fn write(&self, skill_dir: &Path) -> Result<(), LockfileError> {
        fs::create_dir_all(skill_dir)?;
        let yaml = serde_yaml_ng::to_string(self)?;
        let final_path = Self::path(skill_dir);
        let tmp = skill_dir.join(format!(".{FILE_NAME}.tmp"));
        fs::write(&tmp, yaml)?;
        fs::rename(tmp, final_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn empty_when_missing() {
        let d = tempdir().unwrap();
        let l = SkillLock::read(d.path()).unwrap();
        assert_eq!(l.schema_version, SCHEMA_VERSION);
        assert!(l.locked.is_empty());
    }

    #[test]
    fn round_trip() {
        let d = tempdir().unwrap();
        let mut l = SkillLock {
            schema_version: SCHEMA_VERSION,
            locked: BTreeMap::new(),
            installed_at: "2026-05-25T00:00:00Z".into(),
        };
        l.locked.insert("web-browsing".into(), "1.2.0".into());
        l.locked.insert("data-table-export".into(), "0.6.1".into());
        l.write(d.path()).unwrap();
        let back = SkillLock::read(d.path()).unwrap();
        assert_eq!(back.locked["web-browsing"], "1.2.0");
        assert_eq!(back.installed_at, l.installed_at);
    }

    #[test]
    fn corrupt_yaml_returns_parse_err() {
        let d = tempdir().unwrap();
        fs::write(SkillLock::path(d.path()), "this is :: not yaml :: at all").unwrap();
        assert!(matches!(
            SkillLock::read(d.path()),
            Err(LockfileError::Parse(_))
        ));
    }
}
