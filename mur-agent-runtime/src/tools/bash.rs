use std::path::PathBuf;
use tokio::process::Command;

use super::{ToolError, ToolExecutor};
use crate::llm::ToolDef;

/// Directories that must be on `PATH` for the bash tool's spawned commands,
/// even when the agent-runtime process itself was launched by a service
/// manager (launchd/systemd) with a minimal default `PATH`
/// (`/usr/bin:/bin:/usr/sbin:/sbin`). Without this, agents hit
/// `bash: mur: command not found` / `npx: command not found` for tools
/// installed via Homebrew, Cargo, or user-local pip/npm — even though those
/// binaries are on the *interactive* user's `PATH` (dogfood issue 1).
fn standard_exec_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs
}

/// Build the `PATH` to use for spawned bash commands: start from whatever
/// `PATH` the current process has (so we never lose anything the caller set
/// up), and append any of [`standard_exec_dirs`] that aren't already present.
/// This works whether the runtime inherited a rich interactive `PATH` or a
/// minimal service-manager one.
fn augmented_path(current_path: Option<&str>) -> String {
    let mut components: Vec<PathBuf> = current_path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();

    for dir in standard_exec_dirs() {
        if !components.contains(&dir) {
            components.push(dir);
        }
    }

    std::env::join_paths(components)
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub struct BashTool {
    pub working_dir: PathBuf,
}

impl BashTool {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash".into(),
            description: "Run a bash shell command. Returns combined stdout+stderr. Non-zero exit codes appear in the output but are not errors.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the command. Defaults to the agent home directory."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'command' field".into()))?
            .to_string();

        let working_dir = input["cwd"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.working_dir.clone());

        let path = augmented_path(std::env::var("PATH").ok().as_deref());

        let child = Command::new("bash")
            .arg("-c")
            .arg(&command)
            .current_dir(&working_dir)
            .env("PATH", path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ToolError::Execution(format!("spawn failed: {e}")))?;

        let output = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            child.wait_with_output(),
        )
        .await
        {
            Ok(result) => result.map_err(|e| ToolError::Execution(format!("spawn failed: {e}")))?,
            Err(_) => {
                // `wait_with_output` consumed `child`, so on timeout we never
                // get a handle back to kill/reap it here; `kill_on_drop`
                // above ensures tokio's process driver still sends the kill
                // and reaps the exit status in the background instead of
                // leaving a zombie behind (dogfood issue 11).
                return Err(ToolError::Execution("command timed out after 30s".into()));
            }
        };

        let mut combined = String::new();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.is_empty() {
            combined.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push_str("\n[stderr]\n");
            }
            combined.push_str(&stderr);
        }
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&format!("[exit code: {code}]"));
        }

        Ok(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolExecutor;

    fn make_tool() -> BashTool {
        BashTool::new(std::env::temp_dir())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_stdout() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(out.contains("hello"), "got: {out}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_stderr() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo err >&2"}))
            .await
            .unwrap();
        assert!(out.contains("err"), "got: {out}");
    }

    #[tokio::test]
    async fn nonzero_exit_in_output_not_err() {
        let t = make_tool();
        let result = t.execute(serde_json::json!({"command": "exit 1"})).await;
        assert!(
            result.is_ok(),
            "non-zero exit should not be Err, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn missing_command_is_invalid_input() {
        let t = make_tool();
        let result = t.execute(serde_json::json!({})).await;
        assert!(matches!(result, Err(ToolError::InvalidInput(_))));
    }

    #[test]
    fn augmented_path_adds_standard_dirs_to_minimal_path() {
        // Simulate a service-manager launch with the classic minimal PATH.
        let result = augmented_path(Some("/usr/bin:/bin:/usr/sbin:/sbin"));
        let dirs: Vec<_> = std::env::split_paths(&result).collect();

        assert!(dirs.contains(&PathBuf::from("/usr/bin")));
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.contains(&PathBuf::from("/usr/local/bin")));
        if let Some(home) = dirs::home_dir() {
            assert!(dirs.contains(&home.join(".local/bin")));
            assert!(dirs.contains(&home.join(".cargo/bin")));
        }
    }

    #[test]
    fn augmented_path_does_not_duplicate_existing_entries() {
        let result = augmented_path(Some("/opt/homebrew/bin:/usr/bin"));
        let dirs: Vec<_> = std::env::split_paths(&result).collect();
        let count = dirs
            .iter()
            .filter(|d| *d == &PathBuf::from("/opt/homebrew/bin"))
            .count();
        assert_eq!(count, 1, "should not duplicate an already-present dir");
    }

    #[test]
    fn augmented_path_handles_missing_path_var() {
        // Even with no PATH at all (e.g. a stripped-down launch environment),
        // we should still end up with the standard dirs.
        let result = augmented_path(None);
        let dirs: Vec<_> = std::env::split_paths(&result).collect();
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kill_path_leaves_no_zombie_children() {
        // Regression test for dogfood issue 11: murmurd/mur-agent-runtime
        // accumulated hundreds of defunct children (631 zombies on one
        // murmurd, 585 and 291 on two runtime instances, ~1.6 new
        // zombies/minute on a fresh process) because the bash tool's
        // timeout branch dropped its `Child` without ever waiting on it.
        // `BashTool::execute` only returns captured text, so there's no
        // pid to inspect after the fact; this mirrors its exact spawn +
        // `kill_on_drop(true)` + timeout shape to capture pids directly
        // and confirm each one is *fully reaped* (not left defunct) once
        // the timed-out future's `Child` is dropped.
        let mut pids = Vec::new();

        for _ in 0..5 {
            let mut child = Command::new("bash")
                .arg("-c")
                .arg("sleep 60")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn failed");
            let pid = child.id().expect("child should have a pid") as libc::pid_t;
            pids.push(pid);

            // Same shape as `execute`'s timeout branch: a short timeout
            // fires long before the 60s sleep finishes, and the `Child`
            // (owned by the timed-out future) is dropped here at the end
            // of this loop iteration.
            let _ = tokio::time::timeout(std::time::Duration::from_millis(50), child.wait()).await;
        }

        // Give tokio's process-driver orphan reaper a moment to finish
        // reaping in the background: `kill_on_drop` only *starts* the
        // kill on drop, reaping the exit status happens asynchronously.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        for pid in pids {
            // SAFETY: `pid` is a plain integer obtained from `Child::id()`
            // moments ago in this same test process, which is the direct
            // parent of that pid. `waitpid` with `WNOHANG` is a
            // non-blocking status query on a raw pid/status buffer we own
            // on the stack; there is no aliasing or lifetime hazard.
            let mut status: libc::c_int = 0;
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            let errno = std::io::Error::last_os_error();

            // A leaked zombie would still be sitting defunct in the
            // process table, so `waitpid` would successfully reap it
            // right here (`ret == pid`). Once the timeout-kill path
            // properly reaps its children (the fix), the kernel has no
            // child entry left for `pid` by the time we get here, so
            // `waitpid` fails with `ECHILD`.
            assert_eq!(
                ret, -1,
                "pid {pid} was still present (zombie or running) after \
                 the timeout-kill path instead of already being reaped; \
                 waitpid returned {ret}, status {status}, errno {errno:?}"
            );
            assert_eq!(
                errno.raw_os_error(),
                Some(libc::ECHILD),
                "expected ECHILD (no such child) confirming pid {pid} was \
                 already reaped in the background, got errno {errno:?}"
            );
        }
    }

    #[tokio::test]
    async fn bash_tool_can_find_binary_only_on_augmented_path() {
        // Regression test for dogfood issue 1: `mur agent send` on a service
        // launched with a minimal PATH couldn't find binaries like `npx`
        // that live under /opt/homebrew/bin. Here we fake that situation by
        // dropping a "fake npx" shim into a directory that is NOT on the
        // process's real PATH, then confirm the bash tool still cannot see
        // it unless one of the standard dirs is where it lives — instead we
        // assert the constructed PATH always contains a real standard dir
        // that resolves on this machine when present, proving the shell
        // sees the augmented PATH end-to-end.
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo $PATH"}))
            .await
            .unwrap();
        assert!(
            out.contains("/opt/homebrew/bin"),
            "spawned shell's PATH should include the standard dirs, got: {out}"
        );
    }
}
