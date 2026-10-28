use std::path::{Path, PathBuf};

use mur_common::agent_facts::who_can_exec;

use super::denial::{
    classify_write_denial, spawn_denied_hint, spawn_denied_path, write_denied_hint,
    write_denied_path,
};

use super::{DenialScope, ToolError, ToolExecutor, ToolOutput, ToolStatus};
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

/// The tool's registered name. B0 (#2181) and the registry key off this.
pub const BASH: &str = "bash";

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
    /// Per-agent scratch dir exported as `TMPDIR`/`TMP`/`TEMP`. `None`
    /// (tests, embedded uses, underivable path) leaves the inherited value.
    pub scratch_dir: Option<PathBuf>,
    /// The runtime-wide job table (spec D3): shared with `bash_wait` and
    /// `bash_kill`, and with `TaskRunner` for deadline/cancel cleanup.
    pub jobs: std::sync::Arc<crate::tools::bash_jobs::JobTable>,
    /// `rm` → trash (issue #2154). `None` = `hooks.trash_guard: false`, or a
    /// test/embedded use: removers run as-is.
    pub trash: Option<super::bash_trash::BashTrash>,
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
            scratch_dir: None,
            jobs: crate::tools::bash_jobs::JobTable::new(),
            trash: None,
        }
    }

    /// Route the agent's `rm` / `rmdir` / `unlink` into its trash.
    pub fn with_trash(mut self, trash: Option<super::bash_trash::BashTrash>) -> Self {
        self.trash = trash;
        self
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

    /// Attach the per-agent scratch dir exported as `TMPDIR`/`TMP`/`TEMP`.
    pub fn with_scratch_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.scratch_dir = dir;
        self
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
        BASH
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: BASH.into(),
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
            // Inside a turn track the adopted directory is the track's
            // counterpart, not the literal argument — run there.
            Some(cwd) => self.session_cwd.set(PathBuf::from(cwd)),
            // No explicit cwd: fall back to the current session base (which
            // starts at the agent home and only moves on an explicit cwd).
            None => self.session_cwd.current(),
        };

        // Never let a command detach a turn track the runtime still owns.
        if let Some(why) =
            super::bash_guard::guard(&command, &working_dir, &self.session_cwd.live_tracks())
        {
            return Err(ToolError::InvalidInput(why));
        }

        // A refusal above wins; only then may a delete become a trash move.
        if let Some(trash) = &self.trash
            && let Some(out) = super::bash_trash::intercept(trash, &command, &working_dir).await
        {
            return Ok(out);
        }

        let timeout_secs = resolve_timeout_secs(
            input
                .get("timeout_secs")
                .or_else(|| input.get("wait_secs"))
                .and_then(serde_json::Value::as_i64),
        );

        // The child is spawned with a cleared env; this allowlist is all it
        // inherits from the runtime (see `bash_env`).
        let mut env =
            super::bash_env::base_env(augmented_path(std::env::var("PATH").ok().as_deref()));
        // Turn track: exec-granted CARGO_TARGET_DIR (+ warm clone). Before the vault so a user secret wins.
        let (target, warm_note) = super::bash_track::prepare_current_turn(
            &self.session_cwd,
            self.agent.as_ref(),
            &command,
        )
        .await;
        env.extend(target);
        // Values leave the vault only here, straight into the child's
        // environment. The model never sees them: the pump masks every chunk
        // before it reaches the tail or the spool (D10).
        if let Some(vault) = &self.secrets {
            env.extend(vault.env_pairs());
        }
        // Later `.envs` entries win, so this overrides an allowlisted `TMPDIR`
        // the runtime itself was launched with.
        if let Some(d) = &self.scratch_dir {
            if std::env::var_os("TMPDIR").is_some_and(|v| v != d.as_os_str()) {
                tracing::debug!(scratch = %d.display(), "overriding inherited TMPDIR");
            }
            crate::agent_paths::extend_scratch_env(&mut env, d);
        }
        // Parallel first compiles race to start the agent's own server (#1860).
        crate::agent_paths::sccache_server::ensure_for(&command, &env).await;
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
        let out = self.finish_poll(poll, &working_dir, false);
        Ok(super::bash_track::with_note(out, warm_note))
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
            // cwd is resolved BEFORE the parse, not after: bash echoes the
            // command as the model wrote it, so a denied `./x` arrives
            // relative and only the session cwd can turn it into a path the
            // hint (and `who_can_exec`) can act on.
            let cwd = working_dir
                .canonicalize()
                .unwrap_or(working_dir.to_path_buf());
            if let Some(bin) = spawn_denied_path(exit.code, &poll.new_stderr, &cwd)
                && let Some((mur_home, agent)) = &self.agent
            {
                let routes = who_can_exec(mur_home, agent, &bin, Some(&cwd));
                let hint = spawn_denied_hint(&bin, agent, &routes);
                combined.push_str(&hint);
                status = ToolStatus::Denied {
                    detail: hint,
                    // The sandbox denied a PATH or a BINARY, not `bash`.
                    // Withdrawing the tool here is what turned one denied
                    // exec into a whole lost turn (2026-09-13).
                    scope: DenialScope::Action,
                };
            } else if let Some(hint) = self.explain_write_denial(&poll.new_stderr, working_dir) {
                combined.push_str(&hint);
                status = ToolStatus::Denied {
                    detail: hint,
                    // The sandbox denied a PATH or a BINARY, not `bash`.
                    // Withdrawing the tool here is what turned one denied
                    // exec into a whole lost turn (2026-09-13).
                    scope: DenialScope::Action,
                };
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
#[path = "bash_tests.rs"]
mod tests;
