//! #1795: the per-call allowlist gate on an unreadable `config.yaml`.

use super::*;

fn tool(home: &std::path::Path) -> FleetRunTool {
    FleetRunTool {
        mur_home: home.to_path_buf(),
        agent_name: "mur".into(),
        signing: None,
        key_version: 0,
        session_cwd: None,
    }
}

/// #1795: an unreadable `config.yaml` (e.g. replaced by rename after a
/// Landlock seal pinned the old inode) used to read as "empty allowlist"
/// and blame the user's allowlist. It must still refuse — the gate stays
/// fail-closed — but name the read failure and the restart.
#[tokio::test]
async fn unreadable_config_refuses_with_a_read_error_not_an_allowlist_hint() {
    let tmp = tempfile::tempdir().unwrap();
    // A directory where the file should be: read_to_string fails with an
    // error other than NotFound, on every platform.
    std::fs::create_dir_all(tmp.path().join("config.yaml")).unwrap();
    let err = tool(tmp.path())
        .execute(serde_json::json!({"fleet": "deep-research"}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::NotAuthorized(_)),
        "fail-closed: {err}"
    );
    let msg = err.to_string();
    assert!(msg.contains("could not re-read"), "{msg}");
    assert!(msg.contains("restart the agent"), "{msg}");
    assert!(
        !msg.contains("must add them"),
        "must not blame the allowlist: {msg}"
    );
}

/// Same for a file that is present but does not parse: refusing is right,
/// telling the user to add entries they already have is not.
#[tokio::test]
async fn unparsable_config_refuses_with_a_read_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.yaml"), "fleet_run: [unclosed\n").unwrap();
    let err = tool(tmp.path())
        .execute(serde_json::json!({"fleet": "deep-research"}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::NotAuthorized(_)),
        "fail-closed: {err}"
    );
    assert!(err.to_string().contains("could not re-read"), "{err}");
}
