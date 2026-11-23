use super::*;
use mur_common::agent::{AgentProfile, SpawnMode};
use mur_common::entitlements_pin::write_pin;
use mur_common::fleet::CONCIERGE_AGENT;

fn concierge_profile(mur_home: &Path) -> (PathBuf, AgentProfile) {
    let home = mur_home.join("agents").join(CONCIERGE_AGENT);
    std::fs::create_dir_all(&home).unwrap();
    let yaml = include_str!("../../tests/fixtures/profile_minimal.yaml");
    let mut profile: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    profile.name = CONCIERGE_AGENT.to_string();
    profile.entitlements.processes.spawn.mode = SpawnMode::None;
    profile.entitlements.processes.spawn.allowed = vec!["git".into()];
    std::fs::write(
        home.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();
    write_pin(mur_home, CONCIERGE_AGENT, &profile.entitlements).unwrap();
    (home, profile)
}

fn mode_on_disk(home: &Path) -> SpawnMode {
    let yaml = std::fs::read_to_string(home.join("profile.yaml")).unwrap();
    serde_yaml_ng::from_str::<AgentProfile>(&yaml)
        .unwrap()
        .entitlements
        .processes
        .spawn
        .mode
}

#[test]
fn concierge_migration_runs_for_the_installed_home() {
    let tmp = tempfile::tempdir().unwrap();
    let (home, _) = concierge_profile(tmp.path());
    migrate_concierge_spawn_mode(&home, tmp.path());
    assert_eq!(mode_on_disk(&home), SpawnMode::Allowlist);
}

#[test]
fn concierge_migration_skips_a_cache_home() {
    let tmp = tempfile::tempdir().unwrap();
    let (installed, _) = concierge_profile(tmp.path());
    let cache = tmp.path().join("cache").join(CONCIERGE_AGENT);
    migrate_concierge_spawn_mode(&cache, tmp.path());
    assert_eq!(mode_on_disk(&installed), SpawnMode::None);
    assert!(
        !tmp.path()
            .join("entitlement-pins")
            .join(format!("{CONCIERGE_AGENT}.concierge-spawn-mode-v1.done"))
            .exists()
    );
}

#[test]
fn concierge_migration_skips_a_purged_home() {
    let tmp = tempfile::tempdir().unwrap();
    let (installed, _) = concierge_profile(tmp.path());
    let purged = tmp.path().join("agents").join(".purged.concierge.1");
    migrate_concierge_spawn_mode(&purged, tmp.path());
    assert_eq!(mode_on_disk(&installed), SpawnMode::None);
}

#[test]
fn bundled_grant_migration_skips_a_cache_home_and_a_purged_home() {
    // `migrate_bundled_spawn_grant` reads the profile of the name it derives;
    // a skipped home must never reach that read, so no profile exists here and
    // an attempt would only log a warning. Observable: no pin or marker dir.
    let tmp = tempfile::tempdir().unwrap();
    for home in [
        tmp.path().join("cache").join("agent_a"),
        tmp.path().join("agents").join(".purged.agent_a.1"),
    ] {
        migrate_bundled_spawn_grant(&home, tmp.path());
    }
    assert!(!tmp.path().join("agents").join("agent_a").exists());
    assert!(!tmp.path().join("entitlement-pins").exists());
}
