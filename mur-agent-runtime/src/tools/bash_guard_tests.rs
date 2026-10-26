//! `refuse_track_detach` table plus the wired-in `bash` refusal.

use super::*;

fn live() -> (PathBuf, Vec<PathBuf>) {
    let project = PathBuf::from("/w/repo");
    let track = project.join(".worktrees/turn-1");
    (project, vec![track])
}

#[test]
fn refuses_detaching_a_live_track() {
    let (project, live) = live();
    for cmd in [
        "git worktree remove .worktrees/turn-1",
        "git worktree remove --force .worktrees/turn-1",
        "git worktree remove /w/repo/.worktrees/turn-1",
        "git worktree remove './.worktrees/turn-1'",
        "git worktree move .worktrees/turn-1 /elsewhere",
        "git -C /w/repo worktree remove .worktrees/turn-1",
        "git -c core.x=1 worktree remove .worktrees/turn-1",
        "GIT_DIR=x /usr/bin/git worktree remove .worktrees/turn-1",
        "cd /w && ls; git worktree remove repo/.worktrees/turn-1",
        "git worktree remove .worktrees/x/../turn-1",
    ] {
        let cwd = if cmd.contains("repo/.worktrees") && !cmd.contains("/w/repo") {
            PathBuf::from("/w")
        } else {
            project.clone()
        };
        let why = refuse_track_detach(cmd, &cwd, &live);
        assert!(
            why.as_deref()
                .is_some_and(|w| w.contains("/w/repo/.worktrees/turn-1")),
            "{cmd:?} → {why:?}"
        );
    }
}

#[test]
fn leaves_everything_else_alone() {
    let (project, live) = live();
    for cmd in [
        "git worktree remove .worktrees/turn-2",
        "git worktree list",
        "git worktree add .worktrees/turn-9",
        "git worktree prune",
        "echo git worktree remove .worktrees/turn-1",
        "git status",
        "",
    ] {
        assert_eq!(refuse_track_detach(cmd, &project, &live), None, "{cmd:?}");
    }
    let none: Vec<PathBuf> = Vec::new();
    assert_eq!(
        refuse_track_detach("git worktree remove .worktrees/turn-1", &project, &none),
        None
    );
}

#[cfg(unix)]
mod runtime {
    use crate::tools::ToolExecutor;
    use crate::tools::bash::BashTool;
    use crate::tools::bash_jobs::CURRENT_TASK_ID;
    use crate::tools::fs_policy::SessionCwd;

    const TURN: &str = "t-guard";

    #[tokio::test]
    async fn bash_refuses_and_never_runs_the_command() {
        let tmp = tempfile::tempdir().unwrap();
        let project = std::fs::canonicalize(tmp.path()).unwrap();
        let track = project.join(".worktrees/turn-g");
        std::fs::create_dir_all(&track).unwrap();
        let marker = project.join("ran");
        let cwd = SessionCwd::new(project.clone());
        cwd.begin_turn(TURN, None, Some(project.clone()));
        cwd.enter_track(TURN, project.clone(), track.clone());
        let tool = BashTool::new(project.clone(), cwd);
        let cmd = format!(
            "touch {} && git worktree remove --force {}",
            marker.display(),
            track.display()
        );
        let res = CURRENT_TASK_ID
            .scope(
                TURN.to_string(),
                tool.execute(serde_json::json!({ "command": cmd })),
            )
            .await;
        let err = res.expect_err("must refuse").to_string();
        assert!(err.contains("live turn track"), "{err}");
        assert!(!marker.exists(), "refused command must not run");
        assert!(track.is_dir());
    }
}
