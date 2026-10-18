use super::*;
use mur_common::agent::McpServerEntry;
use mur_common::entitlements_pin::{check, write_pin};

const AGENT: &str = "agent_a";

struct Fx {
    _dir: tempfile::TempDir,
    mur_home: std::path::PathBuf,
    bundled: std::path::PathBuf,
}

impl Fx {
    fn profile_path(&self) -> std::path::PathBuf {
        self.mur_home
            .join("agents")
            .join(AGENT)
            .join("profile.yaml")
    }
    fn reload(&self) -> AgentProfile {
        serde_yaml_ng::from_str(&std::fs::read_to_string(self.profile_path()).unwrap()).unwrap()
    }
    fn allowed(&self) -> Vec<String> {
        self.reload().entitlements.processes.spawn.allowed
    }
    fn abs(&self) -> String {
        self.bundled.to_string_lossy().into_owned()
    }
    fn run(&self) -> Migration {
        migrate_bundled_spawn_grant(&self.mur_home, AGENT, &self.bundled).unwrap()
    }
}

/// An installed agent with one MCP entry running `command` and the given
/// spawn allowlist, pinned as a trusted writer would leave it.
fn fixture(command: &str, allowed: &[&str], edit: impl FnOnce(&mut AgentProfile)) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let mur_home = dir.path().to_path_buf();
    let bundled = mur_home.join("mcp-servers").join("mur-mcp-server");
    let home = mur_home.join("agents").join(AGENT);
    std::fs::create_dir_all(&home).unwrap();

    let yaml = include_str!("../tests/fixtures/profile_minimal.yaml");
    let mut profile: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    profile.mcp_servers = vec![McpServerEntry {
        name: "mur".into(),
        command: command.into(),
        ..Default::default()
    }];
    profile.entitlements.processes.spawn.allowed = allowed.iter().map(|s| s.to_string()).collect();
    edit(&mut profile);
    std::fs::write(
        home.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();
    write_pin(&mur_home, AGENT, &profile.entitlements).unwrap();
    Fx {
        _dir: dir,
        mur_home,
        bundled,
    }
}

#[test]
fn grants_the_absolute_path_beside_a_bare_name_grant_and_advances_the_pin() {
    let fx = fixture("mur-mcp-server", &["mur-mcp-server"], |_| {});
    assert_eq!(fx.run(), Migration::Granted);
    assert_eq!(fx.allowed(), vec!["mur-mcp-server".to_string(), fx.abs()]);
    // The runtime's #712 check must accept the migrated profile.
    let ent = fx.reload().entitlements;
    assert_eq!(check(&fx.mur_home, AGENT, &ent).unwrap(), PinCheck::Match);
}

#[test]
fn is_idempotent() {
    let fx = fixture("mur-mcp-server", &["mur-mcp-server"], |_| {});
    assert_eq!(fx.run(), Migration::Granted);
    let after_first = std::fs::read_to_string(fx.profile_path()).unwrap();
    assert_eq!(fx.run(), Migration::Unchanged);
    assert_eq!(
        std::fs::read_to_string(fx.profile_path()).unwrap(),
        after_first
    );
}

#[test]
fn leaves_a_profile_whose_entitlements_changed_outside_mur() {
    let fx = fixture("mur-mcp-server", &["mur-mcp-server"], |_| {});
    // Simulate tampering after the pin was written.
    let mut p = fx.reload();
    p.entitlements
        .processes
        .spawn
        .allowed
        .push("/bin/sh".into());
    std::fs::write(fx.profile_path(), serde_yaml_ng::to_string(&p).unwrap()).unwrap();
    let before = std::fs::read_to_string(fx.profile_path()).unwrap();

    assert_eq!(fx.run(), Migration::SkippedUnpinned);
    assert_eq!(std::fs::read_to_string(fx.profile_path()).unwrap(), before);
    // The pin is not laundered: the next start still refuses.
    assert!(matches!(
        check(&fx.mur_home, AGENT, &p.entitlements).unwrap(),
        PinCheck::Mismatch { .. }
    ));
}

#[test]
fn migrates_an_agent_with_no_pin_yet_and_pins_it() {
    let fx = fixture("mur-mcp-server", &["mur-mcp-server"], |_| {});
    std::fs::remove_file(entitlements_pin::pin_path(&fx.mur_home, AGENT)).unwrap();
    assert_eq!(fx.run(), Migration::Granted);
    let ent = fx.reload().entitlements;
    assert_eq!(check(&fx.mur_home, AGENT, &ent).unwrap(), PinCheck::Match);
}

#[test]
fn does_not_add_a_grant_the_user_never_gave() {
    // Entry present, but the bare name was never granted (e.g. revoked).
    let fx = fixture("mur-mcp-server", &[], |_| {});
    assert_eq!(fx.run(), Migration::Unchanged);
    assert!(fx.allowed().is_empty());
}

#[test]
fn ignores_a_disabled_entry() {
    let fx = fixture("mur-mcp-server", &["mur-mcp-server"], |p| {
        p.set_mcp_enabled("mur", false)
    });
    assert_eq!(fx.run(), Migration::Unchanged);
    assert_eq!(fx.allowed(), vec!["mur-mcp-server".to_string()]);
}

#[test]
fn ignores_third_party_servers() {
    let fx = fixture("some-other-server", &["some-other-server"], |_| {});
    assert_eq!(fx.run(), Migration::Unchanged);
    assert_eq!(fx.allowed(), vec!["some-other-server".to_string()]);
}

#[test]
fn ignores_spawn_modes_that_do_not_consult_the_allowlist() {
    let fx = fixture("mur-mcp-server", &["mur-mcp-server"], |p| {
        p.entitlements.processes.spawn.mode = SpawnMode::Any
    });
    assert_eq!(fx.run(), Migration::Unchanged);
}

#[test]
fn all_reports_only_the_agents_it_migrated() {
    let fx = fixture("mur-mcp-server", &["mur-mcp-server"], |_| {});
    // A stray directory without a profile must not break the sweep.
    std::fs::create_dir_all(fx.mur_home.join("agents").join("not_an_agent")).unwrap();
    assert_eq!(
        migrate_bundled_spawn_grant_all(&fx.mur_home, &fx.bundled),
        vec![AGENT.to_string()]
    );
    assert!(migrate_bundled_spawn_grant_all(&fx.mur_home, &fx.bundled).is_empty());
}
