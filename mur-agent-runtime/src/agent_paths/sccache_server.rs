//! Start the agent's own sccache server before a build needs it.
//!
//! [`super::cache_env`] gives every agent its own `SCCACHE_SERVER_UDS`, so no
//! server is listening there until a build starts one. When the first build
//! fans out into parallel C compiles (CMake for `whisper-rs-sys`'s ggml), each
//! compile tries to start the server at once and the loser fails with
//! `Server startup failed: File exists (os error 17)`. Starting it once, up
//! front, removes the race. The probe runs before every build-like command,
//! so a server that exited on sccache's idle timeout is started again next
//! time instead of being kept alive forever after the agent stops.

use std::path::Path;
use std::time::Duration;

/// Words that mark a command as one that may compile through sccache.
const BUILD_COMMAND_WORDS: &[&str] = &["cargo", "sccache"];

/// Upper bound on `sccache --start-server`; a hung start must not hold the
/// user's command forever.
const START_TIMEOUT: Duration = Duration::from_secs(15);

const SOCKET_KEY: &str = "SCCACHE_SERVER_UDS";

/// Whether `command` runs one of [`BUILD_COMMAND_WORDS`] as a program.
pub fn wants_server(command: &str) -> bool {
    command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .filter_map(|w| Path::new(w).file_name()?.to_str())
        .any(|w| BUILD_COMMAND_WORDS.contains(&w))
}

/// The socket the child will use. Later entries win, as with `Command::envs`.
pub fn socket_from_env(env: &[(String, String)]) -> Option<&str> {
    env.iter()
        .rev()
        .find(|(k, _)| k == SOCKET_KEY)
        .map(|(_, v)| v.as_str())
}

/// Start the sccache server on the child's socket when `command` looks like a
/// build and nothing is listening yet. Best effort: no sccache on `PATH`, a
/// failed start or a timeout is logged and the command runs as before.
#[cfg(unix)]
pub async fn ensure_for(command: &str, env: &[(String, String)]) {
    if !wants_server(command) {
        return;
    }
    let Some(socket) = socket_from_env(env) else {
        return;
    };
    if std::os::unix::net::UnixStream::connect(socket).is_ok() {
        return;
    }
    let start = tokio::process::Command::new("sccache")
        .arg("--start-server")
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(START_TIMEOUT, start).await {
        Ok(Ok(out)) if out.status.success() => {
            tracing::debug!(socket, "started the agent's sccache server");
        }
        Ok(Ok(out)) => tracing::warn!(
            socket,
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "sccache --start-server failed"
        ),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(Err(e)) => tracing::warn!(socket, error = %e, "could not run sccache"),
        Err(_) => tracing::warn!(socket, "sccache --start-server timed out"),
    }
}

#[cfg(not(unix))]
pub async fn ensure_for(_command: &str, _env: &[(String, String)]) {}

#[cfg(all(test, unix))]
#[path = "sccache_server_tests.rs"]
mod tests;
