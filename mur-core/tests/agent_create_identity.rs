// Hardcodes a `/tmp/...` path for the runtime stub binary which doesn't exist
// on Windows. Gate to Unix matching the pattern used by other Unix-only tests
// in this crate.
#![cfg(unix)]

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
        mur_common::identity::private_key_dir(&agent_dir)
            .join("identity.key")
            .exists(),
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

    // M1.3: bootstrap rotation attestation
    let rotations_path = agent_dir.join("rotations.jsonl");
    assert!(
        rotations_path.exists(),
        "rotations.jsonl missing after agent create"
    );
    let content = std::fs::read_to_string(&rotations_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one bootstrap line, got {}",
        lines.len()
    );
    let v: serde_json::Value =
        serde_json::from_str(lines[0]).expect("bootstrap line must be valid JSON");
    assert_eq!(v["bootstrap"], true);
    assert_eq!(v["old_key_version"], 0);
    assert_eq!(v["new_key_version"], 0);
    assert_eq!(v["algorithm"], "ed25519");
    assert_eq!(v["old_pubkey"], "");
    assert!(v["new_pubkey"].as_str().unwrap().starts_with('z'));
    assert!(
        v.get("signature")
            .map(|s| s.as_str().unwrap_or("").is_empty())
            .unwrap_or(true),
        "bootstrap line must have empty/missing signature"
    );

    // M1.3: profile.yaml carries created_at_key + algorithm + key_version=0
    assert!(
        yaml.contains("algorithm: ed25519"),
        "profile must declare algorithm"
    );
    assert!(
        yaml.contains("created_at_key:"),
        "profile must record created_at_key"
    );
    // key_version is 0 — serde may emit `key_version: 0` literally
    assert!(
        yaml.contains("key_version: 0"),
        "profile must declare key_version 0"
    );
}
