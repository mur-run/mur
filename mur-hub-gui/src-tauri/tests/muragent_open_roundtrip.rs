//! Phase D harness: load a `.muragent` through the **Hub GUI** open path
//! (`install_muragent_file`, the same Tauri command the app invokes on a
//! `.muragent` file association) and assert the bundled system prompt + skill
//! files land in the recipient home. Exercises the batch-2 export-fidelity fix
//! through the GUI surface, not just the CLI.

use std::fs;

use mur_common::AgentProfile;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};

#[test]
fn hub_gui_open_muragent_lands_prompt_and_skills() {
    let tmp = tempfile::TempDir::new().unwrap();

    // Build a signed .muragent carrying a system prompt + a skill file.
    let mut profile = AgentProfile::default_for_tests();
    profile.name = "hubbot".into();
    let manifest = build_manifest_from_profile(&profile, env!("CARGO_PKG_VERSION"));
    let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let mut writer = MuragentWriter::new(manifest, profile_yaml, AgentIdentity::generate());
    writer.set_sys_prompt("You are HubBot, opened via the Hub GUI.".into());
    writer.add_skill("greet.md", b"# greet\nsay hi".to_vec());
    let pkg = tmp.path().join("hubbot.muragent");
    writer.write(&pkg).unwrap();

    // Point the install at an isolated home and invoke the GUI command.
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    // SAFETY: single-threaded test; trust::mur_home() reads MUR_HOME.
    unsafe { std::env::set_var("MUR_HOME", &home) }

    // The `install_muragent_file` command only reads the Hub version off an
    // AppHandle before delegating here; a test has no AppHandle, so it calls
    // the seam with an explicit version.
    let receipt = mur_hub_gui_lib::import_muragent::install_inner(&pkg, "0.0.0-test")
        .expect("Hub GUI install path should succeed");

    let agent_dir = home.join("agents").join(&receipt.slug);
    assert_eq!(
        fs::read_to_string(agent_dir.join("sys_prompt.md")).unwrap(),
        "You are HubBot, opened via the Hub GUI.",
        "system prompt must survive the Hub GUI open path"
    );
    assert_eq!(
        fs::read_to_string(agent_dir.join("skills").join("greet.md")).unwrap(),
        "# greet\nsay hi",
        "skill backing file must survive the Hub GUI open path"
    );

    unsafe { std::env::remove_var("MUR_HOME") }
}
