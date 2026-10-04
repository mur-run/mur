//! `mur doctor` must resolve its root from `$MUR_HOME`, like every other
//! surface, instead of hardcoding `~/.mur` (#1692).

use std::process::Command;

#[test]
fn doctor_reports_mur_home_not_home_dot_mur() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join("custom-mur");
    let fake_home = tmp.path().join("home");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::create_dir_all(&fake_home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .arg("doctor")
        .env("MUR_HOME", &mur_home)
        .env("HOME", &fake_home)
        .env("MUR_KEYCHAIN_DISABLED", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run mur doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);

    let expected = format!("MUR directory: {}", mur_home.display());
    assert!(
        stdout.contains(&expected),
        "want `{expected}`, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("MUR directory not found"),
        "doctor ignored MUR_HOME:\n{stdout}"
    );
    assert!(
        !stdout.contains(&fake_home.join(".mur").display().to_string()),
        "doctor fell back to $HOME/.mur:\n{stdout}"
    );
}
