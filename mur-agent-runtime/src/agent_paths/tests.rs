use super::*;
use std::path::{Path, PathBuf};

#[test]
fn scratch_dir_is_mur_home_tmp_agent() {
    let got = agent_scratch_dir(Path::new("/m/agents/a")).unwrap();
    assert_eq!(got, PathBuf::from("/m/tmp/a"));
}

#[test]
fn root_has_no_mur_home() {
    assert!(matches!(
        agent_scratch_dir(Path::new("/")),
        Err(AgentPathError::NoMurHome(_))
    ));
}

#[test]
fn bare_relative_name_is_an_error() {
    assert!(agent_scratch_dir(Path::new("a")).is_err());
}

#[test]
fn scratch_env_sets_all_three_keys_to_the_dir() {
    let env = scratch_env(Path::new("/m/tmp/a"));
    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["TMPDIR", "TMP", "TEMP"]);
    assert!(env.iter().all(|(_, v)| v == "/m/tmp/a"));
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn fresh_scratch_dir_and_parent_are_0700() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("tmp").join("a");
    ensure_scratch_dir(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&home.path().join("tmp")), 0o700);
}

#[cfg(unix)]
#[test]
fn existing_0755_dirs_are_tightened_to_0700() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let parent = home.path().join("tmp");
    let dir = parent.join("a");
    std::fs::create_dir_all(&dir).unwrap();
    for p in [&parent, &dir] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    ensure_scratch_dir(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&parent), 0o700);
}
