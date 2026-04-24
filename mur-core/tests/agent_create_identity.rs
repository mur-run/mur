use mur_common::identity::AgentIdentity;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn agent_create_generates_identity_and_writes_into_profile() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    let runtime_target = "/tmp/mur-agent-runtime-stub-for-identity-test";

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_AGENT_RUNTIME_BIN", runtime_target)
        .args(["agent", "create", "test_identity_agent", "--no-interactive"])
        .output()
        .expect("run mur");
    assert!(
        out.status.success(),
        "mur agent create failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let agent_dir = mur_home.path().join("agents/test_identity_agent");
    assert!(
        agent_dir.join("identity.key").exists(),
        "identity.key missing"
    );
    assert!(
        agent_dir.join("identity.pub").exists(),
        "identity.pub missing"
    );

    // Roundtrip — loaded identity.pub must match the derived pubkey.
    let id = AgentIdentity::load(&agent_dir).expect("load identity");
    let pub_text = std::fs::read_to_string(agent_dir.join("identity.pub")).unwrap();
    assert_eq!(pub_text.trim(), id.pubkey_text());

    // Profile has an identity block with a z-prefixed (base58btc) pubkey.
    let yaml = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert!(
        yaml.contains("identity:"),
        "profile missing identity block: {yaml}"
    );
    assert!(
        yaml.contains("pubkey: z"),
        "profile missing z-prefixed pubkey: {yaml}"
    );
}
