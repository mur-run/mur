//! Integration tests for `mur agent rekey` (M1.4).

use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn mur_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mur"))
}

fn runtime_bin() -> PathBuf {
    let mur = mur_bin();
    mur.parent()
        .expect("mur has parent")
        .join("mur-agent-runtime")
}

fn create_agent(home: &std::path::Path, name: &str) {
    let status = Command::new(mur_bin())
        .env("MUR_HOME", home)
        .env("MUR_AGENT_BIN_DIR", home.join("bin"))
        .env("MUR_AGENT_RUNTIME_BIN", runtime_bin())
        .args(["agent", "create", name, "--no-interactive"])
        .status()
        .expect("spawn mur create");
    assert!(status.success(), "agent create failed");
}

fn rekey(home: &std::path::Path, name: &str, reason: &str) {
    let out = Command::new(mur_bin())
        .env("MUR_HOME", home)
        .env("MUR_AGENT_BIN_DIR", home.join("bin"))
        .env("MUR_AGENT_RUNTIME_BIN", runtime_bin())
        .args(["agent", "rekey", name, "--reason", reason, "--yes"])
        .output()
        .expect("spawn mur rekey");
    assert!(
        out.status.success(),
        "rekey failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn rekey_advances_key_version_and_writes_prev() {
    let home = TempDir::new().unwrap();
    create_agent(home.path(), "rekey_basic");
    let agent_dir = home.path().join("agents/rekey_basic");

    let yaml_before = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert!(yaml_before.contains("key_version: 0"));
    let pubkey_before = std::fs::read_to_string(agent_dir.join("identity.pub")).unwrap();

    rekey(home.path(), "rekey_basic", "scheduled");

    // Profile updated
    let yaml = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert!(
        yaml.contains("key_version: 1"),
        "profile must advance to key_version 1: {yaml}"
    );
    assert!(yaml.contains("previous_pubkey:"));
    assert!(yaml.contains("grace_expires_at:"));
    assert!(yaml.contains("rotated_at:"));

    // identity.pub differs
    let pubkey_after = std::fs::read_to_string(agent_dir.join("identity.pub")).unwrap();
    assert_ne!(
        pubkey_before.trim(),
        pubkey_after.trim(),
        "pubkey must change"
    );

    // .prev files exist
    assert!(
        agent_dir.join("identity.key.prev").exists(),
        "identity.key.prev missing"
    );
    assert!(
        agent_dir.join("identity.pub.prev").exists(),
        "identity.pub.prev missing"
    );

    // attestation file exists + has signature
    let att_str = std::fs::read_to_string(agent_dir.join("identity.attestation.json")).unwrap();
    let att: serde_json::Value = serde_json::from_str(&att_str).unwrap();
    assert_eq!(att["old_key_version"], 0);
    assert_eq!(att["new_key_version"], 1);
    assert!(att["signature"].as_str().unwrap().starts_with('z'));

    // jsonl has 2 lines
    let jsonl = std::fs::read_to_string(agent_dir.join("rotations.jsonl")).unwrap();
    let lines: Vec<&str> = jsonl.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "jsonl should have bootstrap + 1 rotation");
}

#[test]
fn double_rekey_advances_to_v2_with_chained_previous_pubkey() {
    let home = TempDir::new().unwrap();
    create_agent(home.path(), "rekey_double");
    let agent_dir = home.path().join("agents/rekey_double");

    let pubkey_v0 = std::fs::read_to_string(agent_dir.join("identity.pub")).unwrap();
    let pubkey_v0 = pubkey_v0.trim().to_string();

    rekey(home.path(), "rekey_double", "scheduled");
    let pubkey_v1 = std::fs::read_to_string(agent_dir.join("identity.pub")).unwrap();
    let pubkey_v1 = pubkey_v1.trim().to_string();
    assert_ne!(pubkey_v0, pubkey_v1);

    rekey(home.path(), "rekey_double", "scheduled");
    let pubkey_v2 = std::fs::read_to_string(agent_dir.join("identity.pub")).unwrap();
    let pubkey_v2 = pubkey_v2.trim().to_string();
    assert_ne!(pubkey_v1, pubkey_v2);

    let yaml = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert!(yaml.contains("key_version: 2"));
    // previous_pubkey should reflect v1, not v0
    assert!(
        yaml.contains(&format!("previous_pubkey: {pubkey_v1}")),
        "profile previous_pubkey should == v1, yaml: {yaml}"
    );

    // jsonl has 3 lines (bootstrap + 2 rotations)
    let jsonl = std::fs::read_to_string(agent_dir.join("rotations.jsonl")).unwrap();
    let lines: Vec<&str> = jsonl.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3);
}

#[test]
fn rekey_with_suspect_compromise_reason_records_in_jsonl() {
    let home = TempDir::new().unwrap();
    create_agent(home.path(), "rekey_reason");
    let agent_dir = home.path().join("agents/rekey_reason");

    rekey(home.path(), "rekey_reason", "suspect-compromise");

    let jsonl = std::fs::read_to_string(agent_dir.join("rotations.jsonl")).unwrap();
    let last_line = jsonl.lines().filter(|l| !l.is_empty()).last().unwrap();
    let v: serde_json::Value = serde_json::from_str(last_line).unwrap();
    assert_eq!(v["reason"], "suspect_compromise");
}

#[test]
fn rekey_emergency_flag_errors_in_m1() {
    let home = TempDir::new().unwrap();
    create_agent(home.path(), "rekey_emerg");

    let out = Command::new(mur_bin())
        .env("MUR_HOME", home.path())
        .env("MUR_AGENT_BIN_DIR", home.path().join("bin"))
        .env("MUR_AGENT_RUNTIME_BIN", runtime_bin())
        .args(["agent", "rekey", "rekey_emerg", "--emergency", "--yes"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "emergency must fail in M1");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("M4"), "error must mention M4: {stderr}");
}
