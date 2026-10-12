//! `command_triggers` table and the first-cargo warm path.

use super::*;

fn cargo() -> Vec<String> {
    vec!["cargo".to_string()]
}

#[test]
fn command_triggers_table() {
    for (cmd, want) in [
        ("cargo build", true),
        ("FOO=1 cargo test", true),
        ("cd x && cargo b", true),
        ("/usr/local/bin/cargo b", true),
        ("ls; cargo check", true),
        ("echo cargo", false),
        ("make", false),
        ("cargotest", false),
        ("", false),
    ] {
        assert_eq!(command_triggers(cmd, &cargo()), want, "{cmd:?}");
    }
}

#[cfg(unix)]
mod runtime {
    use crate::tools::ToolExecutor;
    use crate::tools::bash::BashTool;
    use crate::tools::bash_jobs::CURRENT_TASK_ID;
    use crate::tools::fs_policy::SessionCwd;
    use std::path::{Path, PathBuf};

    const TURN: &str = "t-warm";
    const TRACK_NAME: &str = "turn-w";
    const NOTE: &str = "[warm build cache";
    const CLONED: &str = "[warm build cache: cloned";

    /// A Cargo project with a fake main target and turn `TURN` in its track.
    /// `min_free_gib: 0` so the test machine's disk never matters.
    fn setup() -> (tempfile::TempDir, PathBuf, BashTool) {
        let tmp = tempfile::tempdir().unwrap();
        let project = std::fs::canonicalize(tmp.path()).unwrap().join("proj");
        let track = project.join(".worktrees").join(TRACK_NAME);
        std::fs::create_dir_all(&track).unwrap();
        std::fs::create_dir_all(project.join("target/debug/.fingerprint")).unwrap();
        std::fs::create_dir_all(project.join("target/debug/deps")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("config.yaml"),
            "turn_track:\n  warm_build_cache:\n    min_free_gib: 0\n",
        )
        .unwrap();
        let cwd = SessionCwd::new(project.clone());
        cwd.begin_turn(TURN, None, Some(project.clone()));
        cwd.enter_track(TURN, project.clone(), track);
        let tool = BashTool::new(project.clone(), cwd).with_agent(home, "a".to_string());
        (tmp, project, tool)
    }

    async fn run(tool: &BashTool, cmd: &str) -> String {
        let input = serde_json::json!({ "command": cmd });
        CURRENT_TASK_ID
            .scope(TURN.to_string(), tool.execute(input))
            .await
            .unwrap()
            .text
    }

    fn turn_dir(project: &Path) -> PathBuf {
        project.join("target/turns").join(TRACK_NAME)
    }

    #[tokio::test]
    async fn first_cargo_warms_once() {
        let (_tmp, project, tool) = setup();
        let first = run(&tool, "cargo --version >/dev/null 2>&1; true").await;
        let second = run(&tool, "cargo --version >/dev/null 2>&1; true").await;
        if cfg!(any(target_os = "macos", target_os = "linux")) && first.contains(NOTE) {
            assert!(turn_dir(&project).join("debug/deps").is_dir());
        }
        assert!(
            !second.contains(NOTE),
            "second call must not warm: {second}"
        );
    }

    #[tokio::test]
    async fn non_trigger_does_not_warm() {
        let (_tmp, project, tool) = setup();
        let out = run(&tool, "echo hi").await;
        assert!(!out.contains(NOTE), "{out}");
        assert!(!turn_dir(&project).exists());
    }
}
