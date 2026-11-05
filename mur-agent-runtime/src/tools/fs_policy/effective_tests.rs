//! Channel consult C2 (§D3.6): an agent's effective file-tool policy has one
//! constructor, and it can be built for another agent from that agent's own
//! `profile.yaml`.

use super::*;

const FIXTURE: &str = include_str!("../../../../mur-common/tests/fixtures/minimal_profile.yaml");

/// `<tmp>/agents/<name>/profile.yaml` with `read` set to `read_root`.
fn agent_home(tmp: &Path, name: &str, read_root: &Path) -> PathBuf {
    let home = tmp.join("agents").join(name);
    std::fs::create_dir_all(&home).unwrap();
    // Single-quoted: YAML double quotes treat `\` as an escape, which breaks
    // on a Windows path (`C:\Users\...`). Single quotes only escape `'`.
    let quoted = read_root.display().to_string().replace('\'', "''");
    let yaml = FIXTURE.replace(
        "filesystem: { read: [], write: [],",
        &format!("filesystem: {{ read: ['{quoted}'], write: [],"),
    );
    assert_ne!(yaml, FIXTURE, "fixture shape changed");
    std::fs::write(home.join("profile.yaml"), yaml).unwrap();
    home
}

#[test]
fn file_tool_policy_is_for_file_tools_of_the_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agents/a");
    let raw = FilesystemEntitlement {
        read: vec!["/srv/docs".into()],
        ..Default::default()
    };
    let (fs, chain) = file_tool_policy(raw.clone(), &home);
    let want = for_file_tools(raw, &home);
    assert_eq!(
        (&fs.read, &fs.write, &fs.deny),
        (&want.read, &want.write, &want.deny)
    );
    assert_eq!(chain.agent_self_home(), home.as_path());
}

#[test]
fn policy_of_another_agent_comes_from_its_own_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let docs = root.join("docs");
    let home = agent_home(&root, "pm", &docs);
    let (fs, chain) = file_tool_policy_of(&home).expect("loads");
    assert!(
        fs.read.iter().any(|r| Path::new(r) == docs),
        "{:?}",
        fs.read
    );
    // The implicit grant the asker's own tools get, not the raw profile.
    let home_s = home.to_string_lossy().into_owned();
    assert!(fs.write.contains(&home_s), "{:?}", fs.write);
    assert_eq!(chain.agent_self_home(), home.as_path());
}

#[test]
fn missing_profile_is_an_error_not_an_empty_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agents/ghost");
    std::fs::create_dir_all(&home).unwrap();
    assert!(matches!(
        file_tool_policy_of(&home),
        Err(crate::profile::ProfileLoadError::NotFound(_))
    ));
}

#[test]
fn unparsable_profile_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("agents/broken");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("profile.yaml"), "schema: [not a profile").unwrap();
    assert!(file_tool_policy_of(&home).is_err());
}
