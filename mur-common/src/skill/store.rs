use crate::skill::manifest::SkillManifest;
use crate::skill::parser::{
    parse_canonical, parse_legacy_markdown, parse_markdown, serialize_canonical, ParseError,
};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Parse(ParseError),
    NotFound(PathBuf),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "io: {e}"),
            StoreError::Parse(e) => write!(f, "parse: {e}"),
            StoreError::NotFound(p) => write!(f, "skill not found: {}", p.display()),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

impl From<ParseError> for StoreError {
    fn from(e: ParseError) -> Self {
        StoreError::Parse(e)
    }
}

pub fn global_skill_dir(mur_home: &Path, name: &str) -> PathBuf {
    mur_home.join("skills").join(name)
}

pub fn agent_skill_dir(mur_home: &Path, agent: &str) -> PathBuf {
    mur_home.join("agents").join(agent).join("skills")
}

pub fn read_from_dir(dir: &Path) -> Result<SkillManifest, StoreError> {
    let yaml = dir.join("skill.yaml");
    if yaml.exists() {
        let text = fs::read_to_string(&yaml)?;
        return Ok(parse_canonical(&text)?);
    }
    let md = dir.join("skill.md");
    if md.exists() {
        let text = fs::read_to_string(&md)?;
        return Ok(parse_markdown(&text)?);
    }
    let legacy = dir.with_extension("md");
    if legacy.exists() {
        let text = fs::read_to_string(&legacy)?;
        return Ok(parse_legacy_markdown(&text)?);
    }
    Err(StoreError::NotFound(dir.to_path_buf()))
}

pub fn write_to_dir(dir: &Path, m: &SkillManifest) -> Result<PathBuf, StoreError> {
    fs::create_dir_all(dir)?;
    let final_path = dir.join("skill.yaml");
    let tmp_path = dir.join(".skill.yaml.tmp");
    let yaml = serialize_canonical(m)?;

    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(yaml.as_bytes())?;
        f.sync_all()?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600))?;
    }

    fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample() -> SkillManifest {
        let yaml = r#"
name: stored
version: 1.0.0
publisher: human:t
description: d
category: context
content:
  abstract: a
  context: b
"#;
        parse_canonical(yaml).unwrap()
    }

    #[test]
    fn write_then_read_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stored");
        write_to_dir(&path, &sample()).unwrap();
        let read = read_from_dir(&path).unwrap();
        assert_eq!(read.name, "stored");
    }

    #[test]
    fn reads_markdown_when_yaml_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("md-skill");
        fs::create_dir_all(&path).unwrap();
        let md = "---\nname: md-skill\nversion: 1.0.0\npublisher: human:t\ndescription: d\ncategory: context\n---\n\nBody content\n";
        fs::write(path.join("skill.md"), md).unwrap();
        let read = read_from_dir(&path).unwrap();
        assert_eq!(read.name, "md-skill");
    }

    #[test]
    fn legacy_dot_md_path_resolves() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mur-context");
        let legacy = "---\nname: mur-context\ndescription: d\n---\n\nbody\n";
        fs::write(dir.path().join("mur-context.md"), legacy).unwrap();
        let read = read_from_dir(&path).unwrap();
        assert_eq!(read.name, "mur-context");
    }

    #[cfg(unix)]
    #[test]
    fn written_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("perm-test");
        let written = write_to_dir(&path, &sample()).unwrap();
        let mode = fs::metadata(&written).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn missing_skill_returns_not_found() {
        let dir = tempdir().unwrap();
        let r = read_from_dir(&dir.path().join("missing"));
        assert!(matches!(r, Err(StoreError::NotFound(_))));
    }
}
