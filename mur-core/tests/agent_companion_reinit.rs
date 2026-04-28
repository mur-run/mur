//! M1.8: re-init preserves ledger / inbox / bandit-state, updates profile.

use std::process::Command;
use tempfile::TempDir;

fn mur_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mur")
}

#[test]
fn re_init_preserves_ledger_inbox_bandit_state() {
    let home = TempDir::new().unwrap();
    let mur_home = home.path().join(".mur");

    // 1. agent create
    let s = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", &mur_home)
        .args([
            "agent",
            "create",
            "darwin",
            "--provider",
            "ollama",
            "--model",
            "llama3.2:3b",
            "--no-interactive",
        ])
        .status()
        .unwrap();
    assert!(s.success());

    // 2. first init (Friend / zh-TW)
    let answers_a = home.path().join("a.yaml");
    std::fs::write(
        &answers_a,
        "locale: zh-TW\nname_for_user: Alice\nrelationship: friend\nformality: casual\nextra_instructions: \"\"\n",
    )
    .unwrap();
    let s = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", &mur_home)
        .args(["agent", "companion", "init", "darwin", "--answers"])
        .arg(&answers_a)
        .status()
        .unwrap();
    assert!(s.success());

    // 3. drop fake ledger/inbox/bandit-state files into companion/
    let comp = mur_home.join("agents/darwin/companion");
    let ledger_dir = comp.join("outbox-ledger");
    std::fs::create_dir_all(&ledger_dir).unwrap();
    let today = chrono::Local::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    std::fs::write(
        ledger_dir.join(format!("{today}.jsonl")),
        "{\"event\":\"CompanionInitialized\",\"at\":\"2026-04-29T00:00:00Z\",\"version\":1}\n",
    )
    .unwrap();
    let inbox_dir = comp.join("inbox");
    std::fs::create_dir_all(&inbox_dir).unwrap();
    std::fs::write(
        inbox_dir.join("01HQTEST.md"),
        "---\nid: 01HQTEST\n---\n\nfake body\n\n>>> response: <unset>\n",
    )
    .unwrap();
    std::fs::write(
        comp.join("bandit-state.json"),
        "{\"version\":1,\"morning_sent_today\":null,\"templates\":{}}",
    )
    .unwrap();

    // 4. re-init (Coach / zh-TW)
    let answers_b = home.path().join("b.yaml");
    std::fs::write(
        &answers_b,
        "locale: zh-TW\nname_for_user: Alice\nrelationship: coach\nformality: casual\nextra_instructions: \"\"\n",
    )
    .unwrap();
    let s = Command::new(mur_bin())
        .env("HOME", home.path())
        .env("MUR_HOME", &mur_home)
        .args(["agent", "companion", "init", "darwin", "--answers"])
        .arg(&answers_b)
        .arg("--re-init")
        .status()
        .unwrap();
    assert!(s.success());

    // 5. assertions: history files preserved, profile.relationship updated to coach
    assert!(
        ledger_dir.join(format!("{today}.jsonl")).exists(),
        "outbox ledger file should be preserved on re-init"
    );
    assert!(
        inbox_dir.join("01HQTEST.md").exists(),
        "inbox markdown should be preserved on re-init"
    );
    assert!(
        comp.join("bandit-state.json").exists(),
        "bandit-state.json should be preserved on re-init"
    );

    let profile_yaml =
        std::fs::read_to_string(mur_home.join("agents/darwin/profile.yaml")).unwrap();
    assert!(
        profile_yaml.contains("relationship: coach"),
        "profile.relationship should now be 'coach'"
    );

    // 6. relationship.json should be rewritten with new values
    let rel: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(comp.join("relationship.json")).unwrap())
            .unwrap();
    assert_eq!(rel["relationship"], "coach");
    assert_eq!(rel["name_for_user"], "Alice");
}
