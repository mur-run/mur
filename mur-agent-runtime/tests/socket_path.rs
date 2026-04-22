use mur_agent_runtime::socket_path::resolve_bind_target;
use tempfile::TempDir;

#[test]
fn short_path_binds_direct_no_symlink() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("x");
    std::fs::create_dir_all(&agent_home).unwrap();
    let uuid = "01JQX4TM8Y9K7VQH6B2N3R5DPE";
    let canonical = agent_home.join("agent.sock");
    let res = resolve_bind_target(&canonical, uuid).unwrap();
    assert_eq!(res.bind_path, canonical);
    assert!(!res.symlink_created, "no symlink expected for short path");
}

#[test]
fn long_path_uses_tmp_and_symlinks() {
    let tmp = TempDir::new().unwrap();
    let long_name = "a".repeat(120);
    let agent_home = tmp.path().join(&long_name).join("agents").join("y");
    std::fs::create_dir_all(&agent_home).unwrap();
    let canonical = agent_home.join("agent.sock");
    let uuid = "01JQX4TM8Y9K7VQH6B2N3R5DPE";
    let res = resolve_bind_target(&canonical, uuid).unwrap();
    assert_ne!(res.bind_path, canonical);
    assert!(res.bind_path.to_string_lossy().starts_with("/tmp/mur-"));
    assert!(
        res.symlink_created,
        "fallback should create a symlink back to canonical"
    );
    let resolved = std::fs::read_link(&canonical).unwrap();
    assert_eq!(resolved, res.bind_path);
}
