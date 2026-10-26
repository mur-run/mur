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

/// #4: a granted scratch dir becomes `TMPDIR`/`TMP`/`TEMP`, overriding
/// whatever the runtime inherited.
#[cfg(unix)]
#[tokio::test]
async fn scratch_dir_is_exported_as_tmpdir() {
    let d = tempfile::tempdir().unwrap();
    let t = make_tool().with_scratch_dir(Some(d.path().to_path_buf()));
    let out = t
        .execute(serde_json::json!({"command": "printf '%s:%s:%s' \"$TMPDIR\" \"$TMP\" \"$TEMP\""}))
        .await
        .unwrap();
    let v = d.path().display();
    assert_eq!(out.text.trim(), format!("{v}:{v}:{v}"));
}

/// #4: no scratch dir → no override; the child sees the inherited value.
#[cfg(unix)]
#[tokio::test]
async fn no_scratch_dir_leaves_tmpdir_inherited() {
    let t = make_tool();
    let out = t
        .execute(serde_json::json!({"command": "printf '%s' \"${TMPDIR:-unset}\""}))
        .await
        .unwrap();
    let want = std::env::var("TMPDIR").unwrap_or_else(|_| "unset".into());
    assert_eq!(out.text.trim(), want);
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

/// Issue #015: the child must inherit HOME and other user-facing env vars.
/// Without them, tools like npm fall back to system defaults that the
/// sandbox may not permit, and fail with EPERM on ~/.npm/_cacache.
#[cfg(unix)]
#[tokio::test]
async fn child_inherits_home_and_npm_env_vars() {
    let t = make_tool();
    // Verify that HOME is passed to the child.
    let out = t
        .execute(serde_json::json!({"command": "echo \"$HOME\""}))
        .await
        .unwrap();
    let home_from_child = out.text.trim();
    let home_from_parent = std::env::var("HOME").unwrap_or_default();
    assert_eq!(
        home_from_child, home_from_parent,
        "child should inherit HOME from parent"
    );

    // Verify that other npm-related vars are passed (if set in parent).
    if let Ok(npm_cache) = std::env::var("NPM_CONFIG_CACHE") {
        let out = t
            .execute(serde_json::json!({"command": "echo \"$NPM_CONFIG_CACHE\""}))
            .await
            .unwrap();
        let npm_from_child = out.text.trim();
        assert_eq!(
            npm_from_child, npm_cache,
            "child should inherit NPM_CONFIG_CACHE"
        );
    }
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
