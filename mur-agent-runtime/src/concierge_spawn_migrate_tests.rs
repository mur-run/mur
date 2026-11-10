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

/// What `mur agent perm set-mode <name> processes.spawn <mode>` leaves behind:
/// the profile rewritten and the pin advanced to it.
fn trusted_set_mode(fx: &Fx, mode: SpawnMode) {
    let mut p = fx.reload();
    p.entitlements.processes.spawn.mode = mode;
    std::fs::write(fx.profile_path(), serde_yaml_ng::to_string(&p).unwrap()).unwrap();
    write_pin(&fx.mur_home, &fx.name, &p.entitlements).unwrap();
}

/// Review regression (#2342): migrate, then the owner revokes the shell on
/// purpose. Every later boot / `mur update` must keep it revoked.
#[test]
fn an_explicit_revocation_after_migration_survives_every_restart() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, true);
    assert_eq!(fx.run(), Migration::Enabled);
    assert!(marker_path(&fx.mur_home, &fx.name).is_file());

    trusted_set_mode(&fx, SpawnMode::None);
    for _ in 0..3 {
        assert_eq!(fx.run(), Migration::Unchanged);
        assert_eq!(fx.mode(), SpawnMode::None);
        assert!(fx.pin_matches());
    }
}

/// An install that never had the bad template is settled on first sight, so a
/// `none` chosen later is never mistaken for it.
#[test]
fn a_profile_already_off_none_is_marked_so_a_later_none_sticks() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::Allowlist, true);
    assert_eq!(fx.run(), Migration::Unchanged);
    assert!(marker_path(&fx.mur_home, &fx.name).is_file());

    trusted_set_mode(&fx, SpawnMode::None);
    assert_eq!(fx.run(), Migration::Unchanged);
    assert_eq!(fx.mode(), SpawnMode::None);
}

/// Skipped for a reseal is still the legacy install: not marked, so the next
/// start after `mur agent perm reseal` repairs it, exactly once.
#[test]
fn a_skipped_profile_is_not_marked_and_is_repaired_after_reseal() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, true);
    let mut p = fx.reload();
    p.entitlements.processes.spawn.allowed.push("curl".into());
    std::fs::write(fx.profile_path(), serde_yaml_ng::to_string(&p).unwrap()).unwrap();

    assert_eq!(fx.run(), Migration::SkippedUnpinned);
    assert!(!marker_path(&fx.mur_home, &fx.name).exists());

    // `mur agent perm reseal mur`.
    write_pin(&fx.mur_home, &fx.name, &p.entitlements).unwrap();
    assert_eq!(fx.run(), Migration::Enabled);
    assert!(marker_path(&fx.mur_home, &fx.name).is_file());

    trusted_set_mode(&fx, SpawnMode::None);
    assert_eq!(fx.run(), Migration::Unchanged);
    assert_eq!(fx.mode(), SpawnMode::None);
}

/// A resumed run settles the profile too.
#[test]
fn a_resumed_run_leaves_the_marker() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::None, true);
    let mut p = fx.reload();
    p.entitlements.processes.spawn.mode = SpawnMode::Allowlist;
    std::fs::write(fx.profile_path(), serde_yaml_ng::to_string(&p).unwrap()).unwrap();

    assert_eq!(fx.run(), Migration::Resumed);
    assert!(marker_path(&fx.mur_home, &fx.name).is_file());
}

/// No profile yet is not "settled": a concierge seeded later is still covered.
#[test]
fn a_missing_profile_leaves_no_marker() {
    let dir = tempfile::tempdir().unwrap();
    migrate_concierge_spawn_mode(dir.path(), CONCIERGE_AGENT).unwrap();
    assert!(!marker_path(dir.path(), CONCIERGE_AGENT).exists());
}

/// `mur agent delete --purge` forgets the pass with the pin.
#[test]
fn forget_rearms_the_pass_for_a_reseeded_concierge() {
    let fx = fixture(CONCIERGE_AGENT, SpawnMode::Allowlist, true);
    fx.run();
    forget(&fx.mur_home, &fx.name).unwrap();
    assert!(!marker_path(&fx.mur_home, &fx.name).exists());
    forget(&fx.mur_home, &fx.name).unwrap(); // already gone: fine

    trusted_set_mode(&fx, SpawnMode::None); // reseeded from an old template
    assert_eq!(fx.run(), Migration::Enabled);
}
