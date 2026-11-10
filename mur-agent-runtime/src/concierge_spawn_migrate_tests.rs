use super::*;
use mur_common::entitlements_pin::{check, write_pin};

struct Fx {
    _dir: tempfile::TempDir,
    mur_home: std::path::PathBuf,
    name: String,
}

impl Fx {
    fn profile_path(&self) -> std::path::PathBuf {
        self.mur_home
            .join("agents")
            .join(&self.name)
            .join("profile.yaml")
    }
    fn reload(&self) -> AgentProfile {
        serde_yaml_ng::from_str(&std::fs::read_to_string(self.profile_path()).unwrap()).unwrap()
    }
    fn mode(&self) -> SpawnMode {
        self.reload().entitlements.processes.spawn.mode
    }
    fn run(&self) -> Migration {
        migrate_concierge_spawn_mode(&self.mur_home, &self.name).unwrap()
    }
    fn pin_matches(&self) -> bool {
        check(&self.mur_home, &self.name, &self.reload().entitlements).unwrap() == PinCheck::Match
    }
}

/// An installed agent `name` with `mode`, pinned as a trusted writer leaves it
/// (unless `pin` is false).
fn fixture(name: &str, mode: SpawnMode, pin: bool) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let mur_home = dir.path().to_path_buf();
    let home = mur_home.join("agents").join(name);
    std::fs::create_dir_all(&home).unwrap();
    let yaml = include_str!("../tests/fixtures/profile_minimal.yaml");
    let mut profile: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    profile.name = name.to_string();
    profile.entitlements.processes.spawn.mode = mode;
    profile.entitlements.processes.spawn.allowed = vec!["git".into()];
    std::fs::write(
        home.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();
    if pin {
        write_pin(&mur_home, name, &profile.entitlements).unwrap();
    }
    Fx {
        _dir: dir,
        mur_home,
        name: name.to_string(),
    }
}

#[test]
fn enables_a_pinned_concierge_and_advances_the_pin() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, true);
    assert_eq!(fx.run(), Migration::Enabled);
    assert_eq!(fx.mode(), SpawnMode::Allowlist);
    // The allowlist is kept, nothing is added to it.
    assert_eq!(
        fx.reload().entitlements.processes.spawn.allowed,
        vec!["git".to_string()]
    );
    // The runtime's #712 check must accept the migrated profile.
    assert!(fx.pin_matches());
}

#[test]
fn is_idempotent() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, true);
    assert_eq!(fx.run(), Migration::Enabled);
    let after_first = std::fs::read_to_string(fx.profile_path()).unwrap();
    assert_eq!(fx.run(), Migration::Unchanged);
    assert_eq!(
        std::fs::read_to_string(fx.profile_path()).unwrap(),
        after_first
    );
}

#[test]
fn migrates_a_concierge_with_no_pin_yet_and_pins_it() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, false);
    assert_eq!(fx.run(), Migration::Enabled);
    assert_eq!(fx.mode(), SpawnMode::Allowlist);
    assert!(fx.pin_matches());
}

#[test]
fn never_touches_other_agents() {
    let fx = fixture("worker", SpawnMode::None, true);
    assert_eq!(fx.run(), Migration::Unchanged);
    assert_eq!(fx.mode(), SpawnMode::None);
}

#[test]
fn leaves_other_modes_alone() {
    for mode in [SpawnMode::Allowlist, SpawnMode::Strict, SpawnMode::Any] {
        let fx = fixture(CONCIERGE_AGENT, mode, true);
        assert_eq!(fx.run(), Migration::Unchanged, "{mode:?}");
        assert_eq!(fx.mode(), mode);
    }
}

#[test]
fn leaves_a_profile_whose_entitlements_changed_outside_mur() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, true);
    // Hand edit after the pin: a new allowlist entry.
    let mut p = fx.reload();
    p.entitlements.processes.spawn.allowed.push("curl".into());
    std::fs::write(fx.profile_path(), serde_yaml_ng::to_string(&p).unwrap()).unwrap();
    let before = std::fs::read_to_string(fx.profile_path()).unwrap();

    assert_eq!(fx.run(), Migration::SkippedUnpinned);
    assert_eq!(std::fs::read_to_string(fx.profile_path()).unwrap(), before);
}

#[test]
fn resumes_a_run_that_wrote_the_profile_but_not_the_pin() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, true);
    // Simulate a crash between the profile write and the pin write.
    let mut p = fx.reload();
    p.entitlements.processes.spawn.mode = SpawnMode::Allowlist;
    std::fs::write(fx.profile_path(), serde_yaml_ng::to_string(&p).unwrap()).unwrap();
    assert!(!fx.pin_matches());

    assert_eq!(fx.run(), Migration::Resumed);
    assert!(fx.pin_matches());
}

#[test]
fn does_not_resume_over_an_unrelated_edit() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, true);
    let mut p = fx.reload();
    p.entitlements.processes.spawn.mode = SpawnMode::Allowlist;
    p.entitlements.processes.spawn.allowed.push("curl".into());
    std::fs::write(fx.profile_path(), serde_yaml_ng::to_string(&p).unwrap()).unwrap();

    assert_eq!(fx.run(), Migration::Unchanged);
    assert!(!fx.pin_matches());
}

#[test]
fn missing_profile_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        migrate_concierge_spawn_mode(dir.path(), CONCIERGE_AGENT).unwrap(),
        Migration::Unchanged
    );
}

/// The Hub seeds the concierge from this template; `none` there is what
/// stranded murmur without `bash` in the first place.
#[test]
fn hub_concierge_template_does_not_ship_spawn_none() {
    let yaml =
        include_str!("../../mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert_ne!(p.entitlements.processes.spawn.mode, SpawnMode::None);
}
