//! M2.3.3 — `mur agent companion preview --no-llm` hydrates `first_memory`
//! from `companion/relationship.json` so that templates referencing
//! `{{FIRST_MEMORY}}` render the actual seed text. Mirrors the runtime-side
//! M2.2.3 wiring on the CLI preview path. Plan §M2.3.3.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn mur_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mur")
}

/// Load the shared P0a fixture and substitute `name: agent_test` →
/// `name: <name>` so the same baseline profile can host any agent.
fn fixture_with_name(name: &str) -> String {
    let yaml = std::fs::read_to_string("../mur-common/tests/fixtures/profile_p0a_minimal.yaml")
        .unwrap_or_else(|_| {
            let p = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("mur-common/tests/fixtures/profile_p0a_minimal.yaml");
            std::fs::read_to_string(p).expect("fixture must exist")
        });
    yaml.replace("name: agent_test", &format!("name: {name}"))
}

#[test]
fn preview_morning_greeting_includes_first_memory() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();

    // Pre-create the agent.
    let agent_dir = mur_home.join("agents/preview_test");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        fixture_with_name("preview_test"),
    )
    .unwrap();

    // Write answers.yaml with a known first_memory.
    let answers_path = tmp.path().join("answers.yaml");
    std::fs::write(
        &answers_path,
        r#"locale: en-US
name_for_user: David
agent_display_name: Mochi
relationship: friend
formality: casual
extra_instructions: ""
first_memory: "Sunday in Taipei"
proactive_tier: warm_only
"#,
    )
    .unwrap();

    // Run `mur agent companion init --answers <path>` end-to-end via the
    // built `mur` binary so the relationship.json on disk is what the
    // preview path will actually read.
    let init = Command::new(mur_bin())
        .env("HOME", tmp.path())
        .env("MUR_HOME", &mur_home)
        .args(["agent", "companion", "init", "preview_test", "--answers"])
        .arg(&answers_path)
        .output()
        .expect("spawn mur agent companion init");
    assert!(
        init.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );

    // Now exercise `mur agent companion preview <name> --situation morning_greeting --no-llm`.
    let preview = Command::new(mur_bin())
        .env("HOME", tmp.path())
        .env("MUR_HOME", &mur_home)
        .args([
            "agent",
            "companion",
            "preview",
            "preview_test",
            "--situation",
            "morning_greeting",
            "--no-llm",
        ])
        .output()
        .expect("spawn mur agent companion preview");
    let body = format!(
        "{}{}",
        String::from_utf8_lossy(&preview.stdout),
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(preview.status.success(), "preview exited non-zero: {body}");
    assert!(
        body.contains("Sunday in Taipei"),
        "preview output should include first_memory; got:\n{body}"
    );
}
