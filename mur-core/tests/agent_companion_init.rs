//! E2E test: `mur agent create` → `mur agent companion init --answers`.
//!
//! Verifies that the non-interactive onboarding wizard mutates `profile.yaml`
//! and writes `companion/relationship.json` atomically. Spec §3.2.

use std::process::Command;
use tempfile::TempDir;

fn mur_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mur")
}

#[test]
fn init_with_answers_writes_profile_and_relationship_json() {
    let home = TempDir::new().unwrap();
    let mur_home = home.path().join(".mur");

    // Pre-create an agent (--no-interactive uses defaults).
    let create = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", &mur_home)
        .args([
            "agent",
            "create",
            "darwin",
            "--no-interactive",
            "--provider",
            "ollama",
            "--model",
            "llama3.2:3b",
        ])
        .output()
        .expect("spawn mur agent create");
    assert!(
        create.status.success(),
        "agent create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );

    // Write answers.yaml.
    let answers = home.path().join("answers.yaml");
    std::fs::write(
        &answers,
        "locale: zh-TW\n\
         name_for_user: David\n\
         relationship: friend\n\
         formality: casual\n\
         extra_instructions: \"\"\n",
    )
    .unwrap();

    // Run companion init.
    let init = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", &mur_home)
        .args(["agent", "companion", "init", "darwin", "--answers"])
        .arg(&answers)
        .output()
        .expect("spawn mur agent companion init");
    assert!(
        init.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );

    // Verify profile.yaml mutated.
    let profile_path = mur_home.join("agents/darwin/profile.yaml");
    let profile_yaml = std::fs::read_to_string(&profile_path).unwrap();
    assert!(
        profile_yaml.contains("relationship: friend"),
        "profile missing relationship: {profile_yaml}"
    );
    assert!(
        profile_yaml.contains("locale: zh-TW"),
        "profile missing locale"
    );
    assert!(
        profile_yaml.contains("name_for_user: David"),
        "profile missing name_for_user"
    );

    // Verify relationship.json shape.
    let rel_path = mur_home.join("agents/darwin/companion/relationship.json");
    let rel_str = std::fs::read_to_string(&rel_path).unwrap();
    let rel: serde_json::Value = serde_json::from_str(&rel_str).unwrap();
    assert_eq!(rel["version"], 1);
    assert_eq!(rel["name_for_user"], "David");
    assert_eq!(rel["relationship"], "friend");
    assert_eq!(rel["locale"], "zh-TW");
    assert_eq!(rel["formality"], "casual");
    assert!(rel["onboarded_at"].is_string());
}

#[test]
fn init_without_answers_or_interactive_fails_clean() {
    // Without --answers AND without a TTY (test harness pipes stdio), the
    // dialoguer wizard fails cleanly on the first prompt. We only assert
    // the process exits non-zero and prints a clear error — not a crash.
    let home = TempDir::new().unwrap();
    let out = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", home.path().join(".mur"))
        .args(["agent", "companion", "init", "darwin"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "init should fail without --answers");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stderr}{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        combined.contains("not a terminal")
            || combined.contains("--answers")
            || combined.contains("read locale")
            || combined.contains("Error:"),
        "expected a clean error (TTY or --answers hint), got: {combined}"
    );
}

#[test]
fn init_refuses_when_already_onboarded_without_re_init() {
    let home = TempDir::new().unwrap();
    let mur_home = home.path().join(".mur");

    // Create + first init.
    let create = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", &mur_home)
        .args([
            "agent",
            "create",
            "darwin",
            "--no-interactive",
            "--provider",
            "ollama",
            "--model",
            "llama3.2:3b",
        ])
        .output()
        .unwrap();
    assert!(create.status.success());

    let answers = home.path().join("answers.yaml");
    std::fs::write(
        &answers,
        "locale: en-US\nname_for_user: A\nrelationship: friend\nformality: neutral\nextra_instructions: \"\"\n",
    )
    .unwrap();

    let first = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", &mur_home)
        .args(["agent", "companion", "init", "darwin", "--answers"])
        .arg(&answers)
        .output()
        .unwrap();
    assert!(first.status.success());

    // Second init without --re-init should refuse.
    let second = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", &mur_home)
        .args(["agent", "companion", "init", "darwin", "--answers"])
        .arg(&answers)
        .output()
        .unwrap();
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("already initialized") || stderr.contains("--re-init"),
        "expected already-initialized message, got: {stderr}"
    );
}
