use std::path::{Path, PathBuf};

use mur_common::agent_facts::who_can_exec;

use super::denial::{
    classify_write_denial, spawn_denied_hint, spawn_denied_path, write_denied_hint,
    write_denied_path,
};

use super::{ToolError, ToolExecutor, ToolOutput, ToolStatus};
use crate::exec_dirs;
use crate::llm::ToolDef;

/// Default wait (in seconds) before `bash` yields a handle when the caller
/// doesn't supply `timeout_secs`.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Upper bound on how long ONE call may hold the turn waiting. It is not a
/// bound on the command: past it the command keeps running and the reply
/// carries a `job_id` (spec 2026-09-12 bash-yield D2). It exists so a turn
/// stays responsive to cancel/steer, nothing else.
const MAX_TIMEOUT_SECS: u64 = 600;

/// Build the `PATH` to use for spawned bash commands: start from whatever
/// `PATH` the current process has (so we never lose anything the caller set
/// up), and append any of [`exec_dirs::standard_exec_dirs`] that aren't
/// already present. This works whether the runtime inherited a rich
/// interactive `PATH` or a minimal service-manager one
/// (launchd/systemd, e.g. `/usr/bin:/bin:/usr/sbin:/sbin`) — without it,
/// agents hit `bash: mur: command not found` / `npx: command not found` for
/// tools installed via Homebrew, Cargo, or user-local pip/npm, even though
/// those binaries are on the *interactive* user's `PATH` (dogfood issue 1).
pub(crate) fn augmented_path(current_path: Option<&str>) -> String {
    let mut components: Vec<PathBuf> = current_path
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();

    for dir in exec_dirs::standard_exec_dirs() {
        if !components.contains(&dir) {
            components.push(dir);
        }
    }

    std::env::join_paths(components)
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub struct BashTool {
    /// Fallback base when no explicit `cwd` is supplied and the session base
    /// has not been overridden.
    pub working_dir: PathBuf,
    /// Session cwd shared with the file tools. An explicit `cwd` argument
    /// updates it; otherwise its current snapshot is used as the base.
    pub session_cwd: crate::tools::fs_policy::SessionCwd,
    /// `(mur_home, canonical agent name)` — consulted ONLY on a kernel exec
    /// denial, to name the delegation route in the error. `None` (tests,
    /// embedded uses) just omits the hint.
    pub agent: Option<(PathBuf, String)>,
    /// The agent's filesystem write grants, as the supervisor resolved them.
    ///
    /// Passed in rather than read back from `profile.yaml`, because the agent
    /// **cannot read its own profile** — the sandbox denies it unconditionally
    /// (`SELF_PROTECTED_AGENT_FILES`, issue #712), so a lookup here returns
    /// `None` inside every real agent and silently costs the explanation this
    /// exists to give.
    pub write_grants: Vec<PathBuf>,
    /// Credentials the user handed the agent, exported into every child's
    /// environment. `None` (tests, embedded uses) exports nothing.
    pub secrets: Option<std::sync::Arc<crate::secrets::SecretVault>>,
    /// The runtime-wide job table (spec D3): shared with `bash_wait` and
    /// `bash_kill`, and with `TaskRunner` for deadline/cancel cleanup.
    pub jobs: std::sync::Arc<crate::tools::bash_jobs::JobTable>,
}

/// `timeout_secs` (alias `wait_secs`): missing/invalid → default; negative →
/// default; `0` → return the handle at once; above the cap → the cap.
fn resolve_timeout_secs(requested: Option<i64>) -> u64 {
    match requested {
        Some(secs) if secs >= 0 => (secs as u64).min(MAX_TIMEOUT_SECS),
        _ => DEFAULT_TIMEOUT_SECS,
    }
}

impl BashTool {
    pub fn new(working_dir: PathBuf, session_cwd: crate::tools::fs_policy::SessionCwd) -> Self {
        Self {
            working_dir,
            session_cwd,
            agent: None,
            write_grants: Vec::new(),
            secrets: None,
            jobs: crate::tools::bash_jobs::JobTable::new(),
        }
    }

    /// Attach the agent identity used to resolve a spawn-denial route.
    pub fn with_agent(mut self, mur_home: PathBuf, agent_name: String) -> Self {
        self.agent = Some((mur_home, agent_name));
        self
    }

    /// The advisory for a filesystem denial in `stderr`, if this can account
    /// for one.
    ///
    /// Separated from `execute` so it is testable, because the way it broke is
    /// not otherwise reachable from a test: it used to look the agent's grants
    /// up from `profile.yaml`, which every real agent is forbidden to read
    /// (issue #712) and every test process can read fine. It passed everywhere
    /// and worked nowhere.
    fn explain_write_denial(&self, stderr: &str, working_dir: &Path) -> Option<String> {
        let denied = write_denied_path(stderr, working_dir)?;
        let (mur_home, agent) = self.agent.as_ref()?;
        let kind = classify_write_denial(
            &self.write_grants,
            &denied,
            &mur_home.join("agents").join(agent),
        )?;
        Some(write_denied_hint(&denied, agent, &kind))
    }

    /// Attach the write grants used to explain a filesystem denial.
    pub fn with_write_grants(mut self, grants: Vec<PathBuf>) -> Self {
        self.write_grants = grants;
        self
    }

    /// Attach the vault whose values become the child's environment.
    pub fn with_secrets(mut self, vault: std::sync::Arc<crate::secrets::SecretVault>) -> Self {
        self.secrets = Some(vault);
        self
    }

    /// Share a job table (production: one per runtime, built in
    /// `supervisor_runner`).
    pub fn with_jobs(mut self, jobs: std::sync::Arc<crate::tools::bash_jobs::JobTable>) -> Self {
        self.jobs = jobs;
        self
    }

    /// The two control tools over this tool's table. Registered together with
    /// `bash` and gated by its policy (`registry::attach_bash_control`).
    pub fn control_tools(self: &std::sync::Arc<Self>) -> Vec<std::sync::Arc<dyn ToolExecutor>> {
        vec![
            std::sync::Arc::new(crate::tools::bash_control::BashWaitTool { bash: self.clone() }),
            std::sync::Arc::new(crate::tools::bash_control::BashKillTool { bash: self.clone() }),
        ]
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
            description: format!(
                "Run a bash shell command. Returns stdout, then stderr under `[stderr]`; a non-zero exit code appears in the output and is not an error. \
Waits up to `timeout_secs` (default {DEFAULT_TIMEOUT_SECS}s, max {MAX_TIMEOUT_SECS}s) for the command to finish. If it is still running after that you get a `job_id` and the output so far — the command KEEPS RUNNING. \
Call `bash_wait` to wait longer, `bash_kill` to stop it. Pass `timeout_secs: 0` to start a command in the background immediately. Never use `nohup` or `&` to work around the wait."
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "What you are doing and why, 5-10 words, active voice, \
                            no trailing period. The command is shown underneath, so do not \
                            restate it: say the intent it cannot show. \
                            Good: \"Checking whether the tag already exists\". \
                            Good: \"Finding which module owns the retry logic\". \
                            Bad: \"Running git tag\" (the command says that). \
                            Bad: \"Executing a shell command\" (says nothing)."
                    },
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the command. Defaults to the session working directory — the one declared under `## Working directory` in your instructions when the client supplied it, else the agent home — so you normally omit this. Passing cwd also moves that shared directory, so a later read_file/write_file/edit_file resolves relative paths against it. NOTE: a `cd` inside the command itself is NOT retained across calls — use this cwd argument instead."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": format!(
                            "Seconds to wait for the command before yielding a job handle (default {DEFAULT_TIMEOUT_SECS}, clamped to 0-{MAX_TIMEOUT_SECS}). \
            The command is NOT killed when this elapses. 0 = return immediately with the handle. `wait_secs` is accepted as an alias."
                        )
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'command' field".into()))?
            .to_string();

        let working_dir = match input["cwd"].as_str() {
            // Explicit cwd: use it AND update the shared session base so a
            // subsequent read_file/write_file/edit_file resolves relative
            // paths against the same directory.
            Some(cwd) => {
                let dir = PathBuf::from(cwd);
                self.session_cwd.set(dir.clone());
                dir
            }
            // No explicit cwd: fall back to the current session base (which
            // starts at the agent home and only moves on an explicit cwd).
            None => self.session_cwd.current(),
        };

        let timeout_secs = resolve_timeout_secs(
            input
                .get("timeout_secs")
                .or_else(|| input.get("wait_secs"))
                .and_then(serde_json::Value::as_i64),
        );

        let mut env = vec![(
            "PATH".to_string(),
            augmented_path(std::env::var("PATH").ok().as_deref()),
        )];
        // Values leave the vault only here, straight into the child's
        // environment. The model never sees them: the pump masks every chunk
        // before it reaches the tail or the spool (D10).
        if let Some(vault) = &self.secrets {
            env.extend(vault.env_pairs());
        }
        let spool_dir = self.working_dir.join("jobs");
        let job_id = self
            .jobs
            .spawn(crate::tools::bash_jobs::SpawnSpec {
                command: &command,
                cwd: &working_dir,
                env,
                spool_dir: &spool_dir,
                vault: self.secrets.clone(),
            })
            .map_err(|e| match e {
                crate::tools::bash_jobs::JobError::Spawn(io) => {
                    let mut msg = format!("spawn failed: {io}");
                    if crate::tools::fs_policy::is_removable_volume_eperm(&working_dir, &io) {
                        msg.push_str("\n\n");
                        msg.push_str(crate::tools::fs_policy::REMOVABLE_VOLUME_EPERM_HINT);
                    }
                    ToolError::Execution(msg)
                }
                too_many @ crate::tools::bash_jobs::JobError::TooMany(_) => {
                    ToolError::InvalidInput(too_many.to_string())
                }
                other => ToolError::Execution(other.to_string()),
            })?;
        let poll = self
            .jobs
            .poll(&job_id, std::time::Duration::from_secs(timeout_secs))
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        Ok(self.finish_poll(poll, &working_dir, false))
    }
}

impl BashTool {
    /// Turn a poll into the model-facing reply. Shared by `bash`, `bash_wait`
    /// and `bash_kill` so the three never disagree about what an exit code,
    /// a denial, or a yield looks like. `killed` = the caller was `bash_kill`.
    pub(crate) fn finish_poll(
        &self,
        poll: crate::tools::bash_jobs::Poll,
        working_dir: &Path,
        killed: bool,
    ) -> ToolOutput {
        let mut combined = String::new();
        if poll.skipped > 0 {
            let where_ = poll
                .spool
                .as_ref()
                .map(|p| format!(" — read_file {}", p.display()))
                .unwrap_or_default();
            combined.push_str(&format!("[… {} bytes not shown{where_}]\n", poll.skipped));
        }
        combined.push_str(&poll.new_stdout);
        if !poll.new_stderr.is_empty() {
            if !combined.is_empty() {
                combined.push_str("\n[stderr]\n");
            }
            combined.push_str(&poll.new_stderr);
        }
        let elapsed = crate::bounds::fmt_dur(poll.elapsed);
        let spool_line = poll
            .spool
            .as_ref()
            .map(|p| format!("; full log: {}", p.display()))
            .unwrap_or_default();
        let note = poll
            .spool_note
            .as_ref()
            .map(|n| format!("\n[{n}]"))
            .unwrap_or_default();

        let Some(exit) = poll.exit else {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&format!(
                "[still running after {elapsed} — job_id: {}; call bash_wait to keep waiting, bash_kill to stop{spool_line}]{note}",
                poll.job_id
            ));
            return ToolOutput {
                text: combined,
                status: ToolStatus::Running {
                    job_id: poll.job_id,
                    bytes_seen: poll.bytes_seen,
                },
                images: Vec::new(),
            };
        };

        let code = exit.code.unwrap_or(-1);
        let mut status = ToolStatus::Ok;
        if killed {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            match exit.killed_by {
                Some(sig) => combined.push_str(&format!(
                    "[killed {} by {sig} after {elapsed}]{note}",
                    poll.job_id
                )),
                None => combined.push_str(&format!(
                    "[{} had already exited with code {code} after {elapsed}]{note}",
                    poll.job_id
                )),
            }
            return ToolOutput {
                text: combined,
                status,
                images: Vec::new(),
            };
        }
        if exit.code != Some(0) {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&format!("[exit code: {code}]"));
            if let Some(bin) = spawn_denied_path(exit.code, &poll.new_stderr)
                && let Some((mur_home, agent)) = &self.agent
            {
                let cwd = working_dir
                    .canonicalize()
                    .unwrap_or(working_dir.to_path_buf());
                let routes = who_can_exec(mur_home, agent, &bin, Some(&cwd));
                let hint = spawn_denied_hint(&bin, agent, &routes);
                combined.push_str(&hint);
                status = ToolStatus::Denied { detail: hint };
            } else if let Some(hint) = self.explain_write_denial(&poll.new_stderr, working_dir) {
                combined.push_str(&hint);
                status = ToolStatus::Denied { detail: hint };
            } else {
                status = ToolStatus::Failed { exit_code: code };
            }
        }
        combined.push_str(&note);
        ToolOutput {
            text: combined,
            status,
            images: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    /// The regression that shipped: the advisory looked the grants up from
    /// `profile.yaml`, which no real agent may read. A `mur_home` with no
    /// profile in it stands in for that — if anything here reads the file, it
    /// finds nothing and stays silent, exactly as it did in production.
    #[test]
    fn the_advisory_does_not_depend_on_reading_the_agents_profile() {
        let empty_home = tempfile::tempdir().unwrap(); // no agents/<name>/profile.yaml
        let tool = BashTool::new(
            PathBuf::from("/repo"),
            crate::tools::fs_policy::SessionCwd::new(PathBuf::from("/repo")),
        )
        .with_agent(empty_home.path().to_path_buf(), "mur".into())
        .with_write_grants(vec![PathBuf::from("/granted")]);

        // The exact line a shell redirect produces under a seatbelt denial.
        let hint = tool
            .explain_write_denial(
                "bash: /Users/d/Documents/probe.txt: Operation not permitted\n",
                Path::new("/repo"),
            )
            .expect("a denial outside every grant must be explained");
        assert!(hint.contains("[sandbox]"), "{hint}");
        assert!(hint.contains("perm allow-write"), "{hint}");
    }

    /// Without grants there is nothing to compare against, so nothing is said —
    /// the embedded/test construction must not start inventing verdicts.
    #[test]
    fn no_grants_means_no_verdict_not_a_guess() {
        let empty_home = tempfile::tempdir().unwrap();
        let tool = BashTool::new(
            PathBuf::from("/repo"),
            crate::tools::fs_policy::SessionCwd::new(PathBuf::from("/repo")),
        )
        .with_agent(empty_home.path().to_path_buf(), "mur".into());
        // No `with_write_grants`: every path is "not granted", which is true
        // but useless, so the hint still names the grant command rather than
        // claiming the path is forbidden.
        let hint = tool.explain_write_denial(
            "bash: /x/y.txt: Operation not permitted\n",
            Path::new("/repo"),
        );
        assert!(hint.is_some_and(|h| h.contains("perm allow-write")));
    }

    /// An ordinary failure still collects nothing, with grants attached.
    #[test]
    fn a_plain_failure_is_still_left_alone() {
        let tool = BashTool::new(
            PathBuf::from("/repo"),
            crate::tools::fs_policy::SessionCwd::new(PathBuf::from("/repo")),
        )
        .with_agent(PathBuf::from("/nowhere"), "mur".into())
        .with_write_grants(vec![PathBuf::from("/granted")]);
        assert_eq!(
            tool.explain_write_denial("error: could not compile\n", Path::new("/repo")),
            None
        );
    }

    use super::*;
    use crate::tools::ToolExecutor;

    fn make_tool() -> BashTool {
        let base = std::env::temp_dir();
        BashTool::new(base.clone(), crate::tools::fs_policy::SessionCwd::new(base))
    }

    #[test]
    fn resolve_timeout_secs_defaults_clamps_and_passes_through() {
        // Absent -> default.
        assert_eq!(resolve_timeout_secs(None), DEFAULT_TIMEOUT_SECS);
        // Non-positive/invalid -> default.
        assert_eq!(
            resolve_timeout_secs(Some(0)),
            0,
            "zero is the background case"
        );
        assert_eq!(resolve_timeout_secs(Some(-5)), DEFAULT_TIMEOUT_SECS);
        // In-range value passes through unchanged.
        assert_eq!(resolve_timeout_secs(Some(120)), 120);
        // Above the ceiling is clamped down to it.
        assert_eq!(resolve_timeout_secs(Some(10_000)), MAX_TIMEOUT_SECS);
        // Exactly at the boundaries.
        assert_eq!(resolve_timeout_secs(Some(1)), 1);
        assert_eq!(
            resolve_timeout_secs(Some(MAX_TIMEOUT_SECS as i64)),
            MAX_TIMEOUT_SECS
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_stdout() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(out.text.contains("hello"), "got: {}", out.text);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn vault_values_reach_the_child_environment() {
        let vault = std::sync::Arc::new(crate::secrets::SecretVault::new());
        vault
            .set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99")
            .unwrap();
        let t = make_tool().with_secrets(vault);
        let out = t
            .execute(serde_json::json!({"command": "printf '%s' \"$GITEA_TOKEN\""}))
            .await
            .unwrap();
        // Masking now happens in the pump (D10) because the spool is
        // model-readable; the runner's chokepoint stays as the guard for
        // every other tool.
        assert_eq!(out.text.trim(), "[SECRET:GITEA_TOKEN]");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn no_vault_means_no_extra_environment() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "printf '%s' \"${GITEA_TOKEN:-unset}\""}))
            .await
            .unwrap();
        assert_eq!(out.text.trim(), "unset");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_stderr() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo err >&2"}))
            .await
            .unwrap();
        assert!(out.text.contains("err"), "got: {}", out.text);
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
    async fn yielded_then_finished_jobs_are_reaped_not_left_defunct() {
        // The yield path leaves no zombie either: the pump reaps the shell.
        // Regression cover for dogfood issue 11 under the new semantics.
        let t = make_tool();
        let mut pids = Vec::new();
        for _ in 0..3 {
            let out = t
                .execute(serde_json::json!({"command": "sleep 0.2", "timeout_secs": 0}))
                .await
                .unwrap();
            let job_id = match out.status {
                ToolStatus::Running { job_id, .. } => job_id,
                other => panic!("expected Running, got {other:?}"),
            };
            pids.push(t.jobs.pid(&job_id).unwrap() as libc::pid_t);
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        for pid in pids {
            let mut status: libc::c_int = 0;
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            assert_eq!(ret, -1, "pid {pid} still a child of this process (zombie)");
        }
    }

    /// D1 at the tool boundary: a yield is `Running`, not an error, and says
    /// so in words the model can act on.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_yields_a_running_status_instead_of_killing() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo start; sleep 3", "timeout_secs": 1}))
            .await
            .unwrap();
        let job_id = match &out.status {
            ToolStatus::Running { job_id, bytes_seen } => {
                assert_eq!(*bytes_seen, 6, "{out:?}");
                job_id.clone()
            }
            other => panic!("expected Running, got {other:?}"),
        };
        assert!(out.text.contains("start"), "{}", out.text);
        assert!(out.text.contains("still running"), "{}", out.text);
        assert!(!out.text.contains("timed out"), "{}", out.text);
        assert!(out.text.contains(&job_id), "{}", out.text);
        assert!(crate::tools::bash_jobs::pid_alive(
            t.jobs.pid(&job_id).unwrap()
        ));
        t.jobs.kill_all().await;
    }

    /// `wait_secs` is an alias for `timeout_secs`.
    #[cfg(unix)]
    #[tokio::test]
    async fn wait_secs_is_an_alias() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "sleep 2", "wait_secs": 0}))
            .await
            .unwrap();
        assert!(matches!(out.status, ToolStatus::Running { .. }), "{out:?}");
        t.jobs.kill_all().await;
    }

    /// A finished command still renders exactly as before the yield existed.
    #[cfg(unix)]
    #[tokio::test]
    async fn finished_command_renders_stdout_then_stderr_then_exit_code() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo out; echo err >&2; exit 3"}))
            .await
            .unwrap();
        assert_eq!(out.status, ToolStatus::Failed { exit_code: 3 });
        assert_eq!(
            out.text, "out\n\n[stderr]\nerr\n\n[exit code: 3]",
            "{}",
            out.text
        );
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
            out.text.contains("/opt/homebrew/bin"),
            "spawned shell's PATH should include the standard dirs, got: {}",
            out.text
        );
    }
}
