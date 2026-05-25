mod common;
use common::TestRegistry;
use mur_common::skill::SkillLock;
use mur_common::trust::skills::SkillTrustStore;
use mur_core::cmd::skill_install::cmd_install;
use std::collections::HashSet;

#[test]
fn install_with_transitive_deps_writes_full_lock_and_trust() {
    let home = tempfile::tempdir().unwrap();
    let reg = TestRegistry::new();
    reg.publish("dep-c", "1.0.0", &[]);
    reg.publish("dep-b", "1.2.0", &[("dep-c", ">=1.0.0")]);
    reg.publish("root", "0.1.0", &[("dep-b", "^1.0.0")]);
    reg.commit();

    cmd_install(home.path(), &reg.url(), "root").expect("install ok");

    // All three skill.yaml files written
    for name in ["root", "dep-b", "dep-c"] {
        assert!(
            home.path()
                .join("skills")
                .join(name)
                .join("skill.yaml")
                .exists(),
            "{name} skill.yaml"
        );
    }

    // skill.lock at root with all three pinned versions
    let lock = SkillLock::read(&home.path().join("skills/root")).unwrap();
    assert_eq!(lock.locked.get("root").map(String::as_str), Some("0.1.0"));
    assert_eq!(lock.locked.get("dep-b").map(String::as_str), Some("1.2.0"));
    assert_eq!(lock.locked.get("dep-c").map(String::as_str), Some("1.0.0"));
    // installed_at — shape, not value
    chrono::DateTime::parse_from_rfc3339(&lock.installed_at).expect("rfc3339");

    // Trust store has all three (look up by name, not hash)
    let trust = SkillTrustStore::load(home.path()).unwrap();
    let names: HashSet<String> = trust.entries.values().map(|e| e.name.clone()).collect();
    let expected: HashSet<String> = ["root", "dep-b", "dep-c"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(names, expected);
}

#[test]
fn cycle_propagates_error_and_leaves_clean_filesystem() {
    let home = tempfile::tempdir().unwrap();
    let reg = TestRegistry::new();
    reg.publish("alpha", "1.0.0", &[("beta", "*")]);
    reg.publish("beta", "1.0.0", &[("alpha", "*")]);
    reg.commit();

    let err = cmd_install(home.path(), &reg.url(), "alpha").unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("cyclic"),
        "expected cyclic-dependency error, got: {err}"
    );

    // Resolver fails before any disk install — neither alpha nor beta should be installed.
    assert!(!home.path().join("skills/alpha/skill.yaml").exists());
    assert!(!home.path().join("skills/beta/skill.yaml").exists());
    assert!(!home.path().join("skills/alpha/skill.lock").exists());
}

#[test]
fn re_install_is_idempotent_and_rewrites_lock() {
    let home = tempfile::tempdir().unwrap();
    let reg = TestRegistry::new();
    reg.publish("solo", "1.0.0", &[]);
    reg.commit();

    cmd_install(home.path(), &reg.url(), "solo").unwrap();
    let lock1 = SkillLock::read(&home.path().join("skills/solo")).unwrap();

    // Second call: must not panic, must produce equivalent end state.
    cmd_install(home.path(), &reg.url(), "solo").unwrap();
    let lock2 = SkillLock::read(&home.path().join("skills/solo")).unwrap();

    assert_eq!(lock1.locked, lock2.locked);
}
