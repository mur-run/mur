//! Turn-track hooks for the `bash` tool (turn-track warm target design).
//!
//! Inside a turn track, Cargo must build into `<project>/target/<subdir>/<track>`:
//! that path sits under the project's `target/`, where the sandbox lets build
//! scripts execute. The track's own `target/` does not, so a build there dies
//! with exit 126 the moment a crate has a `build.rs`.

use crate::tools::fs_policy::SessionCwd;
use mur_common::config::{Config, WarmBuildCacheConfig};
use mur_track::turn::target::turn_target_dir;
use std::path::{Path, PathBuf};

/// The variable Cargo reads for its target dir.
pub const CARGO_TARGET_DIR_ENV: &str = "CARGO_TARGET_DIR";
/// Marks a project as a Cargo project.
const CARGO_MANIFEST: &str = "Cargo.toml";

/// `CARGO_TARGET_DIR=<project>/target/<subdir>/<track-name>` when `project`
/// is a Cargo project, else `None`. The dir is not created: Cargo does that.
pub fn target_env(
    project: &Path,
    track: &Path,
    cfg: &WarmBuildCacheConfig,
) -> Option<(String, String)> {
    if !project.join(CARGO_MANIFEST).is_file() {
        return None;
    }
    let name = track.file_name()?.to_str()?;
    let dir = turn_target_dir(project, cfg.validated_subdir(), name);
    Some((CARGO_TARGET_DIR_ENV.to_string(), dir.display().to_string()))
}

/// [`target_env`] for the calling turn, if it is inside a track. Config comes
/// from `<mur_home>/config.yaml` when the tool knows its agent, else defaults.
pub(crate) fn target_env_for_current_turn(
    session_cwd: &SessionCwd,
    agent: Option<&(PathBuf, String)>,
) -> Option<(String, String)> {
    let id = crate::tools::bash_jobs::current_task_id()?;
    let (project, track) = session_cwd.track_of(&id)?;
    let cfg = agent
        .map(|(home, _)| {
            Config::load_or_default(&home.join("config.yaml"))
                .turn_track
                .warm_build_cache
        })
        .unwrap_or_default();
    target_env(&project, &track, &cfg)
}

#[cfg(all(test, unix))]
mod tests {
    use crate::tools::ToolExecutor;
    use crate::tools::bash::BashTool;
    use crate::tools::bash_jobs::CURRENT_TASK_ID;
    use crate::tools::fs_policy::SessionCwd;
    use std::path::{Path, PathBuf};

    const TURN: &str = "t-cargo";
    const TRACK_NAME: &str = "turn-1";

    /// A project (optionally a Cargo one) with turn `TURN` inside its track.
    fn tool_in_track(cargo: bool) -> (tempfile::TempDir, PathBuf, BashTool) {
        let tmp = tempfile::tempdir().unwrap();
        let project = std::fs::canonicalize(tmp.path()).unwrap();
        let track = project.join(".worktrees").join(TRACK_NAME);
        std::fs::create_dir_all(&track).unwrap();
        if cargo {
            std::fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();
        }
        let cwd = SessionCwd::new(project.clone());
        cwd.begin_turn(TURN, None, Some(project.clone()));
        cwd.enter_track(TURN, project.clone(), track);
        let tool = BashTool::new(project.clone(), cwd);
        (tmp, project, tool)
    }

    async fn echo_target_dir(tool: &BashTool) -> String {
        let cmd = serde_json::json!({"command": "printf '%s' \"${CARGO_TARGET_DIR:-unset}\""});
        CURRENT_TASK_ID
            .scope(TURN.to_string(), tool.execute(cmd))
            .await
            .unwrap()
            .text
            .trim()
            .to_string()
    }

    fn turn_dir(project: &Path) -> String {
        project
            .join("target/turns")
            .join(TRACK_NAME)
            .display()
            .to_string()
    }

    #[tokio::test]
    async fn bash_exports_cargo_target_dir_in_track() {
        let (_tmp, project, tool) = tool_in_track(true);
        assert_eq!(echo_target_dir(&tool).await, turn_dir(&project));
    }

    #[tokio::test]
    async fn bash_no_cargo_target_dir_without_cargo_toml() {
        let (_tmp, project, tool) = tool_in_track(false);
        assert_ne!(echo_target_dir(&tool).await, turn_dir(&project));
    }

    #[tokio::test]
    async fn bash_no_cargo_target_dir_outside_track() {
        let (_tmp, project, tool) = tool_in_track(true);
        tool.session_cwd.leave_track(TURN, project.clone());
        assert_ne!(echo_target_dir(&tool).await, turn_dir(&project));
    }
}
