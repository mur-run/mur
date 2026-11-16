use super::*;
use crate::sandbox::SandboxPolicy;

/// A machine where the CLI has never written global limits or installed a
/// shared skill: neither directory exists when the agent starts. Both read
/// grants are existence-checked and Landlock takes no new rule after the
/// seal, so the pre-seal step has to leave both in the policy's read set, or
/// the first `mur limits --global` / `mur skill install` stays unreadable
/// until the agent restarts.
#[test]
fn stores_the_cli_creates_later_are_readable_on_a_fresh_home() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();

    ensure_read_grant_dirs(mur_home);

    let profile = mur_common::agent::AgentProfile::default_for_tests();
    let policy = SandboxPolicy::from_entitlements(&profile.entitlements, &agent_home);
    for dir in [
        mur_common::limits::global_limits_dir(mur_home),
        mur_common::skill::global_skills_root(mur_home),
    ] {
        assert!(
            policy.fs_read.contains(&dir),
            "{} must be granted on a fresh home, or it is unreadable until restart: {:?}",
            dir.display(),
            policy.fs_read
        );
    }
}
