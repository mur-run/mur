//! Integration test for `mur skill install agent://...` in the M4a
//! single-home reality. The handler/dispatcher wire round-trip is
//! exercised separately in M4b.

use mur_common::agent::AgentProfile;
use mur_common::skill::{
    TrustLevel, content_hash_for_trust, global_skill_dir, parse_canonical, read_from_dir,
    write_to_dir,
};
use mur_common::trust::skills::SkillTrustStore;
use mur_core::cmd::skill_install::cmd_install;
use tempfile::tempdir;

fn write_profile(home: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let profile = AgentProfile {
        name: name.to_string(),
        ..AgentProfile::default_for_tests()
    };
    let yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let path = dir.join("profile.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

#[test]
fn agent_pull_installs_and_appends_transfer_chain() {
    let home = tempdir().unwrap();

    // Source agent "alice" — owns the skill.
    write_profile(home.path(), "alice");
    let manifest = parse_canonical(
        r#"
name: find-prices
version: 1.0.0
publisher: human:alice
description: Find product prices
category: workflow
content:
  abstract: Searches product prices.
  context: "Full procedure."
"#,
    )
    .unwrap();
    write_to_dir(&global_skill_dir(home.path(), "find-prices"), &manifest).unwrap();

    // Target agent "bob" — caller of the install.
    let bob_profile_path = write_profile(home.path(), "bob");

    // SAFETY: env mutation isn't thread-safe across parallel tests. This
    // test file must be run with `--test-threads=1`.
    unsafe { std::env::set_var("MUR_AGENT_NAME", "bob") };

    let result = cmd_install(
        home.path(),
        "https://example.com/registry", // unused for agent:// path
        "agent://alice/find-prices",
    );

    // SAFETY: see comment above.
    unsafe { std::env::remove_var("MUR_AGENT_NAME") };
    result.unwrap();

    // 1. Skill file exists in the shared store.
    let installed_dir = global_skill_dir(home.path(), "find-prices");
    assert!(installed_dir.join("skill.yaml").exists());

    // 2. transfer_chain was appended.
    let installed = read_from_dir(&installed_dir).unwrap();
    assert_eq!(installed.transfer_chain, vec!["agent://alice"]);

    // 3. Trust entry is Sandboxed (no registry cache in this test).
    let trust = SkillTrustStore::load(home.path()).unwrap();
    let key = content_hash_for_trust(&installed).unwrap();
    let entry = trust.lookup(&key).expect("trust entry exists");
    assert!(matches!(entry.level, TrustLevel::Sandboxed));

    // 4. Bob's profile carries the SkillCardEntry.
    let bob_yaml = std::fs::read_to_string(&bob_profile_path).unwrap();
    let bob: AgentProfile = serde_yaml_ng::from_str(&bob_yaml).unwrap();
    assert_eq!(bob.installed_skills.len(), 1);
    let entry = &bob.installed_skills[0];
    assert_eq!(entry.name, "find-prices");
    assert_eq!(entry.publisher, "human:alice");
    assert_eq!(entry.abstract_text, "Searches product prices.");
    assert_eq!(entry.transfer_chain, vec!["agent://alice"]);
}

#[test]
fn agent_url_rejects_missing_source_agent() {
    let home = tempdir().unwrap();
    let err = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://nonexistent/skill",
    )
    .unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn agent_url_rejects_missing_source_skill() {
    let home = tempdir().unwrap();
    write_profile(home.path(), "charlie");
    let err = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://charlie/missing-skill",
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not found") || msg.contains("pull"),
        "unexpected error: {msg}"
    );
}

#[test]
fn agent_url_skips_profile_register_without_caller() {
    let home = tempdir().unwrap();
    write_profile(home.path(), "dave");
    let manifest = parse_canonical(
        r#"
name: solo
version: 1.0.0
publisher: human:dave
description: d
category: context
content:
  abstract: a
  context: b
"#,
    )
    .unwrap();
    write_to_dir(&global_skill_dir(home.path(), "solo"), &manifest).unwrap();

    // No MUR_AGENT_NAME set → install succeeds, no profile mutation.
    cmd_install(home.path(), "https://x", "agent://dave/solo").unwrap();
    let installed = read_from_dir(&global_skill_dir(home.path(), "solo")).unwrap();
    assert_eq!(installed.transfer_chain, vec!["agent://dave"]);
}
