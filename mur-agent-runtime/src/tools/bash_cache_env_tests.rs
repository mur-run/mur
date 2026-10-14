//! #1860: build-tool cache defaults reach the bash child. Lives under `tools/`
//! because only `tools/` may call `ToolExecutor::execute` directly.

use super::bash::BashTool;
use crate::tools::ToolExecutor;

/// #1860, end to end through the real bash tool: with a scratch dir and
/// nothing preset, the child sees the cache defaults.
#[cfg(unix)]
#[tokio::test]
async fn a_bash_child_gets_the_cache_defaults_from_the_scratch_dir() {
    // Held for the whole test: the child inherits this process's environment,
    // so a stray SCCACHE_* from the host would turn "default" into "preset".
    let _env = mur_common::test_env::EnvGuard::unset([
        "SCCACHE_DIR",
        "SCCACHE_SERVER_PORT",
        "SCCACHE_SERVER_UDS",
        "ORT_CACHE_DIR",
    ]);
    let d = tempfile::tempdir().unwrap();
    let base = std::env::temp_dir();
    let tool = BashTool::new(base.clone(), crate::tools::fs_policy::SessionCwd::new(base))
        .with_scratch_dir(Some(d.path().to_path_buf()));
    let out = tool
        .execute(serde_json::json!({
            "command": "printf '%s|%s|%s' \"$SCCACHE_DIR\" \"$SCCACHE_SERVER_UDS\" \"$ORT_CACHE_DIR\""
        }))
        .await
        .unwrap();
    let v = d.path().display();
    assert_eq!(
        out.text.trim(),
        format!("{v}/sccache|{v}/sccache.sock|{v}/ort-cache")
    );
}
