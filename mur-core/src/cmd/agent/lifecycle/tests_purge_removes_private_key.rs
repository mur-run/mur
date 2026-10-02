use super::*;
use tempfile::TempDir;

/// The regression: `--purge` deleted the agent home and left
/// `<mur_home>/keys/<name>/identity.key` behind. Nothing enumerates that
/// tree, so the orphans were only found by counting `keys/` against
/// `agents/` by hand.
#[test]
fn purge_removes_the_key_that_lives_outside_the_agent_home() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("gone");
    let key_dir = tmp.path().join("keys").join("gone");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::create_dir_all(&key_dir).unwrap();
    std::fs::write(key_dir.join("identity.key"), b"not-a-real-key").unwrap();

    let removed = purge_agent_state(&agent_home).unwrap();

    assert_eq!(removed.as_deref(), Some(key_dir.as_path()));
    assert!(!agent_home.exists(), "agent home survived the purge");
    assert!(!key_dir.exists(), "private key outlived its agent");
}

/// An agent whose key was never created must still purge cleanly rather
/// than failing on a missing directory.
#[test]
fn purge_succeeds_when_the_agent_never_had_a_key() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("keyless");
    std::fs::create_dir_all(&agent_home).unwrap();

    assert!(purge_agent_state(&agent_home).unwrap().is_none());
    assert!(!agent_home.exists());
}

/// A directory that is not under `agents/` maps to itself, and must not be
/// removed twice — `private_key_dir` deliberately passes those through.
#[test]
fn purge_does_not_double_remove_a_non_agent_directory() {
    let tmp = TempDir::new().unwrap();
    let stray = tmp.path().join("commander");
    std::fs::create_dir_all(&stray).unwrap();

    assert!(purge_agent_state(&stray).unwrap().is_none());
    assert!(!stray.exists());
}
