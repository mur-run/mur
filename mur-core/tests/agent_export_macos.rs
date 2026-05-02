//! Unit tests for the macOS hardening helpers in agent_export_gui.
//! These DO NOT shell out to notarytool / stapler / spctl — they
//! verify the argv vector each phase would emit.

use mur_core::cmd::agent_export_gui::{NotarizeCreds, notarize_args};
use std::path::Path;

#[test]
fn notarize_args_uses_app_specific_password() {
    let creds = NotarizeCreds {
        apple_id: "alex@example.com".into(),
        team_id: "ABCDE12345".into(),
        password: "abcd-efgh-ijkl-mnop".into(),
    };
    let args = notarize_args(Path::new("/tmp/MurAgent.zip"), &creds);
    assert_eq!(args[0], "notarytool");
    assert_eq!(args[1], "submit");
    assert_eq!(args[2], "/tmp/MurAgent.zip");
    assert!(args.contains(&"--apple-id".to_string()));
    assert!(args.contains(&"alex@example.com".to_string()));
    assert!(args.contains(&"--team-id".to_string()));
    assert!(args.contains(&"ABCDE12345".to_string()));
    assert!(args.contains(&"--password".to_string()));
    assert!(args.contains(&"abcd-efgh-ijkl-mnop".to_string()));
    // --wait so the export pipeline doesn't return until Apple
    // accepts/rejects the submission.
    assert!(args.contains(&"--wait".to_string()));
    // Output format must be `--output-format json` so the parent can
    // parse status if needed; right now we just check the exit code.
    assert!(args.contains(&"--output-format".to_string()));
    assert!(args.contains(&"json".to_string()));
}

#[test]
fn staple_args_targets_the_app_bundle() {
    use mur_core::cmd::agent_export_gui::staple_args;
    let args = staple_args(Path::new("/tmp/MurAgent.app"));
    assert_eq!(args[0], "stapler");
    assert_eq!(args[1], "staple");
    assert_eq!(args[2], "/tmp/MurAgent.app");
}
