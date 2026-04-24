use mur_common::identity::{AgentIdentity, IdentityError};
use tempfile::TempDir;

#[test]
fn generate_roundtrip() {
    let dir = TempDir::new().unwrap();
    let id = AgentIdentity::generate();
    id.save(dir.path()).unwrap();
    let loaded = AgentIdentity::load(dir.path()).unwrap();
    assert_eq!(id.verifying_key_bytes(), loaded.verifying_key_bytes());
}

#[test]
fn load_missing_returns_err() {
    let dir = TempDir::new().unwrap();
    let err = AgentIdentity::load(dir.path()).unwrap_err();
    assert!(matches!(err, IdentityError::NotFound));
}

#[test]
fn private_key_file_is_mode_0600() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let id = AgentIdentity::generate();
        id.save(dir.path()).unwrap();
        let meta = std::fs::metadata(dir.path().join("identity.key")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
