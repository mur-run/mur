//! `bash_wait` and `bash_kill`: the two control tools over `bash`'s job table
//! (spec 2026-09-12 bash-yield D6). Thin on purpose — every reply goes
//! through `BashTool::finish_poll` so the three tools never disagree.

use std::sync::Arc;

use super::bash::BashTool;
use super::{ToolError, ToolExecutor, ToolOutput};
use crate::llm::ToolDef;

pub const BASH_WAIT: &str = "bash_wait";
pub const BASH_KILL: &str = "bash_kill";

/// `bash_wait.wait_secs` default.
const DEFAULT_WAIT_SECS: u64 = 60;
/// Same wait cap as `bash` (D2).
const MAX_WAIT_SECS: u64 = 600;

fn job_id_of(input: &serde_json::Value) -> Result<String, ToolError> {
    input
        .get("job_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidInput("missing 'job_id' field".into()))
}

fn wait_secs_of(input: &serde_json::Value) -> u64 {
    match input
        .get("wait_secs")
        .or_else(|| input.get("timeout_secs"))
        .and_then(serde_json::Value::as_i64)
    {
        Some(s) if s >= 0 => (s as u64).min(MAX_WAIT_SECS),
        _ => DEFAULT_WAIT_SECS,
    }
}

fn job_error(e: crate::tools::bash_jobs::JobError) -> ToolError {
    match e {
        unknown @ crate::tools::bash_jobs::JobError::Unknown(..) => {
            ToolError::InvalidInput(unknown.to_string())
        }
        other => ToolError::Execution(other.to_string()),
    }
}

pub struct BashWaitTool {
    pub bash: Arc<BashTool>,
}

#[async_trait::async_trait]
impl ToolExecutor for BashWaitTool {
    fn name(&self) -> &str {
        BASH_WAIT
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: BASH_WAIT.into(),
            description: format!(
                "Keep waiting on a bash command that is still running (a `job_id` from `bash`). \
Returns the output since the last reply. Waits up to `wait_secs` (default {DEFAULT_WAIT_SECS}, max {MAX_WAIT_SECS}); \
if the command is still running after that you get the handle again — call bash_wait again, or bash_kill to stop it."
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "The job_id a previous bash/bash_wait reply gave you" },
                    "wait_secs": { "type": "integer", "description": format!("Seconds to wait before yielding again (default {DEFAULT_WAIT_SECS}, 0-{MAX_WAIT_SECS})") }
                },
                "required": ["job_id"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let job_id = job_id_of(&input)?;
        let wait = std::time::Duration::from_secs(wait_secs_of(&input));
        let poll = self
            .bash
            .jobs
            .poll(&job_id, wait)
            .await
            .map_err(job_error)?;
        let cwd = self.bash.session_cwd.current();
        Ok(self.bash.finish_poll(poll, &cwd, false))
    }
}

pub struct BashKillTool {
    pub bash: Arc<BashTool>,
}

#[async_trait::async_trait]
impl ToolExecutor for BashKillTool {
    fn name(&self) -> &str {
        BASH_KILL
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: BASH_KILL.into(),
            description: "Stop a running bash command (a `job_id` from `bash`): SIGTERM to its whole process group, then SIGKILL after 2 seconds. Returns its last output.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "The job_id a previous bash/bash_wait reply gave you" }
                },
                "required": ["job_id"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let job_id = job_id_of(&input)?;
        let poll = self.bash.jobs.kill(&job_id).await.map_err(job_error)?;
        let cwd = self.bash.session_cwd.current();
        Ok(self.bash.finish_poll(poll, &cwd, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolStatus;

    fn bash() -> Arc<BashTool> {
        let base = std::env::temp_dir();
        Arc::new(BashTool::new(
            base.clone(),
            crate::tools::fs_policy::SessionCwd::new(base),
        ))
    }

    /// Test 2 at the tool boundary: bash → Running, bash_wait → Ok with the
    /// exit code, a second bash_wait → InvalidInput naming the job.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_wait_collects_the_exit_once() {
        let b = bash();
        let out = b
            .execute(serde_json::json!({"command": "sleep 0.5; echo done", "timeout_secs": 0}))
            .await
            .unwrap();
        let ToolStatus::Running { job_id, .. } = out.status else {
            panic!("{out:?}")
        };
        let wait = BashWaitTool { bash: b.clone() };
        let out = wait
            .execute(serde_json::json!({"job_id": job_id, "wait_secs": 5}))
            .await
            .unwrap();
        assert_eq!(out.status, ToolStatus::Ok, "{out:?}");
        assert_eq!(out.text, "done\n");
        let again = wait.execute(serde_json::json!({"job_id": job_id})).await;
        match again {
            Err(ToolError::InvalidInput(msg)) => assert!(msg.contains(&job_id), "{msg}"),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    /// Test 3 at the tool boundary: kill is `Ok`, names the signal and the
    /// elapsed time, and the pid is gone.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_kill_is_ok_and_names_the_signal() {
        let b = bash();
        let out = b
            .execute(serde_json::json!({"command": "sleep 30", "timeout_secs": 0}))
            .await
            .unwrap();
        let ToolStatus::Running { job_id, .. } = out.status else {
            panic!("{out:?}")
        };
        let pid = b.jobs.pid(&job_id).unwrap();
        let out = BashKillTool { bash: b.clone() }
            .execute(serde_json::json!({"job_id": job_id}))
            .await
            .unwrap();
        assert_eq!(out.status, ToolStatus::Ok, "{out:?}");
        assert!(
            out.text.contains("killed") && out.text.contains("SIG"),
            "{}",
            out.text
        );
        assert!(!crate::tools::bash_jobs::pid_alive(pid));
    }

    #[tokio::test]
    async fn unknown_job_is_invalid_input() {
        let b = bash();
        let r = BashWaitTool { bash: b.clone() }
            .execute(serde_json::json!({"job_id": "j-nope"}))
            .await;
        assert!(matches!(r, Err(ToolError::InvalidInput(_))), "{r:?}");
        let r = BashKillTool { bash: b }
            .execute(serde_json::json!({}))
            .await;
        assert!(matches!(r, Err(ToolError::InvalidInput(_))), "{r:?}");
    }
}
