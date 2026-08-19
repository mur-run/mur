//! Integration tests for `mur agent rekey` (M1.4).
//
// Spawns `mur agent create` which copies a runtime binary into a temp dir;
// on Windows the source path lacks `.exe` and the copy fails. Gate to Unix
// matching the pattern used by the rest of `mur-core/tests/agent_*.rs`.
#![cfg(unix)]

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
        mur_common::identity::private_key_dir(&agent_dir)
            .join("identity.key.prev")
            .exists(),
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
    let last_line = jsonl.lines().rfind(|l| !l.is_empty()).unwrap();
    let v: serde_json::Value = serde_json::from_str(last_line).unwrap();
    assert_eq!(v["reason"], "suspect_compromise");
}

#[test]
fn rekey_emergency_writes_unsigned_attestation() {
    let home = TempDir::new().unwrap();
    create_agent(home.path(), "rekey_emerg");
    let agent_dir = home.path().join("agents/rekey_emerg");

    let out = Command::new(mur_bin())
        .env("MUR_HOME", home.path())
        .env("MUR_AGENT_BIN_DIR", home.path().join("bin"))
        .env("MUR_AGENT_RUNTIME_BIN", runtime_bin())
        .args(["agent", "rekey", "rekey_emerg", "--emergency", "--yes"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "emergency rekey must succeed with --yes: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PENDING APPROVAL"),
        "stdout missing pending warn: {stdout}"
    );

    // Attestation present + signature empty + reason emergency
    let att_str = std::fs::read_to_string(agent_dir.join("identity.attestation.json")).unwrap();
    let att: serde_json::Value = serde_json::from_str(&att_str).unwrap();
    assert_eq!(att["reason"], "emergency");
    let sig = att.get("signature").and_then(|s| s.as_str()).unwrap_or("");
    assert!(
        sig.is_empty(),
        "emergency attestation must have empty signature"
    );

    // Profile carries emergency_rekey_at
    let yaml = std::fs::read_to_string(agent_dir.join("profile.yaml")).unwrap();
    assert!(
        yaml.contains("emergency_rekey_at:"),
        "profile missing emergency_rekey_at: {yaml}"
    );
    assert!(yaml.contains("key_version: 1"));
}

#[test]
fn rekey_emergency_aborts_without_confirmation_phrase() {
    use std::io::Write as _;
    use std::process::Stdio;

    let home = TempDir::new().unwrap();
    create_agent(home.path(), "rekey_emerg_abort");

    // Without --yes, the CLI demands the literal "I UNDERSTAND" phrase.
    // Pipe the wrong text and assert the rotation does NOT happen.
    let mut child = Command::new(mur_bin())
        .env("MUR_HOME", home.path())
        .env("MUR_AGENT_BIN_DIR", home.path().join("bin"))
        .env("MUR_AGENT_RUNTIME_BIN", runtime_bin())
        .args(["agent", "rekey", "rekey_emerg_abort", "--emergency"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(b"yes\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "abort path must exit 0");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("aborted"), "must report abort: {stderr}");

    // Profile must STILL be at key_version 0
    let yaml =
        std::fs::read_to_string(home.path().join("agents/rekey_emerg_abort/profile.yaml")).unwrap();
    assert!(
        yaml.contains("key_version: 0"),
        "rotation must NOT have happened"
    );
}

#[test]
fn rekey_status_shows_initial_v0_state() {
    let home = TempDir::new().unwrap();
    create_agent(home.path(), "rekey_status_a");

    let out = Command::new(mur_bin())
        .env("MUR_HOME", home.path())
        .env("MUR_AGENT_BIN_DIR", home.path().join("bin"))
        .env("MUR_AGENT_RUNTIME_BIN", runtime_bin())
        .args(["agent", "rekey-status", "rekey_status_a"])
        .output()
        .expect("rekey-status spawn");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("key_version:        0"), "stdout: {stdout}");
    assert!(stdout.contains("algorithm:          ed25519"));
    assert!(stdout.contains("rotation log lines: 1"));
    assert!(stdout.contains("previous_pubkey:    <none in grace>"));
}

#[test]
fn rekey_status_shows_after_rotation_with_grace() {
    let home = TempDir::new().unwrap();
    create_agent(home.path(), "rekey_status_b");
    rekey(home.path(), "rekey_status_b", "scheduled");

    let out = Command::new(mur_bin())
        .env("MUR_HOME", home.path())
        .env("MUR_AGENT_BIN_DIR", home.path().join("bin"))
        .env("MUR_AGENT_RUNTIME_BIN", runtime_bin())
        .args(["agent", "rekey-status", "rekey_status_b", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("json output");
    assert_eq!(v["key_version"], 1);
    assert_eq!(v["algorithm"], "ed25519");
    assert!(v["previous_pubkey"].as_str().unwrap_or("").starts_with('z'));
    assert!(v["grace_expires_at"].is_string());
    let remaining = v["grace_remaining_days"].as_i64().expect("grace days");
    assert!(
        (28..=30).contains(&remaining),
        "expected ~30 days, got {remaining}"
    );
    assert_eq!(v["rotation_log_lines"], 2);
}
