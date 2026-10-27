//! `refuse_track_detach` table plus the wired-in `bash` refusal.

use super::*;

const TURN: &str = "turn-a";

fn live() -> (PathBuf, Vec<(String, PathBuf)>) {
    let project = PathBuf::from("/w/repo");
    let track = project.join(".worktrees/turn-1");
    (project, vec![(TURN.to_string(), track)])
}

fn refused(cmd: &str, cwd: &Path, live: &[(String, PathBuf)]) -> Option<Refusal> {
    refuse_track_detach(cmd, cwd, live)
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
        "sudo git worktree remove .worktrees/turn-1",
    ] {
        let r = refused(cmd, &project, &live).unwrap_or_else(|| panic!("{cmd:?} not refused"));
        assert_eq!(r.turn, TURN, "{cmd:?}");
        assert_eq!(r.track, project.join(".worktrees/turn-1"), "{cmd:?}");
    }
}

#[test]
fn refuses_removing_or_gutting_a_live_track() {
    let (project, live) = live();
    for cmd in [
        "rm -rf .worktrees/turn-1",
        "rm -rf -- .worktrees/turn-1",
        "rm -r .worktrees",
        "rm -rf .worktrees/turn-1/*",
        "/bin/rm -fr /w/repo/.worktrees/turn-1/",
        "trash .worktrees/turn-1",
        "rmdir .worktrees/turn-1",
        "cd .worktrees && rm -rf turn-1",
        "mv .worktrees/turn-1 /tmp/gone",
        "env FOO=1 rm -rf .worktrees/turn-1",
    ] {
        let r = refused(cmd, &project, &live).unwrap_or_else(|| panic!("{cmd:?} not refused"));
        assert_eq!(r.turn, TURN, "{cmd:?}");
    }
}

#[test]
fn guard_covers_every_wrapper_and_remover_the_trash_interceptor_knows() {
    // One shared list: adding a wrapper or remover in `mur_trash::detect`
    // must widen the guard too, or the two would disagree on what deletes.
    let (project, live) = live();
    for remover in REMOVERS.iter().chain([&GUARD_ONLY_REMOVER]) {
        let cmd = format!("{remover} .worktrees/turn-1");
        assert!(
            refused(&cmd, &project, &live).is_some(),
            "{cmd:?} not refused"
        );
        for wrapper in WRAPPERS {
            let cmd = format!("{wrapper} {remover} .worktrees/turn-1");
            assert!(
                refused(&cmd, &project, &live).is_some(),
                "{cmd:?} not refused"
            );
        }
    }
}

#[test]
fn leaves_everything_else_alone() {
    let (project, live) = live();
    for cmd in [
        "git worktree remove .worktrees/turn-2",
        "git worktree list",
        "git worktree add .worktrees/turn-9",
        "echo git worktree remove .worktrees/turn-1",
        "echo rm -rf .worktrees/turn-1",
        "rm -rf .worktrees/turn-2",
        "rm -rf .worktrees/turn-1/target",
        "rm .worktrees/turn-1/scratch.txt",
        "mv /tmp/a .worktrees/turn-1/a",
        "git status",
        "",
    ] {
        assert_eq!(refused(cmd, &project, &live), None, "{cmd:?}");
    }
    assert_eq!(
        refused("git worktree remove .worktrees/turn-1", &project, &[]),
        None
    );
}

#[test]
fn refusal_message_names_the_owning_turn_and_track() {
    let (project, live) = live();
    let r = refused("rm -rf .worktrees/turn-1", &project, &live).unwrap();
    let msg = r.message();
    assert!(msg.contains("turn turn-a"), "{msg}");
    // Compare paths by component, not as a `/`-joined literal: on Windows
    // the message renders the track as `\w\repo\.worktrees\turn-1`.
    assert!(r.track.ends_with(Path::new(".worktrees").join("turn-1")));
    assert!(msg.contains(&r.track.display().to_string()), "{msg}");
    assert!(msg.contains("rm -rf .worktrees/turn-1"), "{msg}");
    assert_eq!(r.track_name(), "turn-1");
}

/// On-disk cases: prune, case-folded and symlinked spellings, unsettled tracks.
mod on_disk {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        project: PathBuf,
        track: PathBuf,
        live: Vec<(String, PathBuf)>,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let project = std::fs::canonicalize(tmp.path()).unwrap();
        let track = project.join(".worktrees/turn-live");
        std::fs::create_dir_all(&track).unwrap();
        std::fs::write(track.join(".git"), "gitdir: x\n").unwrap();
        let live = vec![(TURN.to_string(), track.clone())];
        Fixture {
            _tmp: tmp,
            project,
            track,
            live,
        }
    }

    #[test]
    fn prune_is_refused_only_while_a_live_track_has_lost_its_tree() {
        let f = fixture();
        assert_eq!(refused("git worktree prune", &f.project, &f.live), None);
        std::fs::remove_file(f.track.join(".git")).unwrap();
        let r = refused("git worktree prune -v", &f.project, &f.live).expect("refused");
        assert_eq!(r.turn, TURN);
        assert_eq!(
            refused("git worktree prune --dry-run", &f.project, &f.live),
            None
        );
    }

    #[test]
    fn unsettled_track_from_a_previous_run_stays_removable() {
        let f = fixture();
        // Left on disk by a killed run: a real worktree, but no turn is in it.
        let stale = f.project.join(".worktrees/turn-stale");
        std::fs::create_dir_all(&stale).unwrap();
        for cmd in [
            "git worktree remove --force .worktrees/turn-stale",
            "rm -rf .worktrees/turn-stale",
            "git worktree prune",
        ] {
            assert_eq!(refused(cmd, &f.project, &f.live), None, "{cmd:?}");
        }
        // A worktree the turn made itself outside `.worktrees/`.
        let scratch = f.project.join("wt-scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let cmd = format!("git worktree remove {}", scratch.display());
        assert_eq!(refused(&cmd, &f.project, &f.live), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_spelling_does_not_bypass() {
        let f = fixture();
        let link = f.project.join("alias");
        std::os::unix::fs::symlink(&f.track, &link).unwrap();
        assert!(refused("git worktree remove alias", &f.project, &f.live).is_some());
        assert!(refused("rm -rf alias/", &f.project, &f.live).is_some());
    }

    #[test]
    fn case_folded_spelling_does_not_bypass_on_a_case_insensitive_fs() {
        let f = fixture();
        let upper = f.project.join(".WORKTREES/TURN-LIVE");
        if !upper.exists() {
            return; // case-sensitive filesystem: the spelling is a different path
        }
        for cmd in [
            "git worktree remove --force .WORKTREES/TURN-LIVE",
            "rm -rf .Worktrees/Turn-Live",
        ] {
            assert!(refused(cmd, &f.project, &f.live).is_some(), "{cmd:?}");
        }
    }
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
        for cmd in [
            format!(
                "touch {} && git worktree remove --force {}",
                marker.display(),
                track.display()
            ),
            format!("touch {} && rm -rf {}", marker.display(), track.display()),
        ] {
            let res = CURRENT_TASK_ID
                .scope(
                    TURN.to_string(),
                    tool.execute(serde_json::json!({ "command": cmd })),
                )
                .await;
            let err = res.expect_err("must refuse").to_string();
            assert!(err.contains(&format!("live track of turn {TURN}")), "{err}");
            assert!(!marker.exists(), "refused command must not run");
            assert!(track.is_dir());
        }
    }
}
