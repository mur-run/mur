//! `lang` validation for `ast_grep_search` (phase 0, item 5).
//!
//! `ast-grep run -p x -l <L> --stdin` with empty stdin exits 1 for a
//! supported language (aliases such as `rs` / `ts` included, case-insensitive)
//! and 2 for an unsupported one. The probe runs under the same isolation as a
//! search (`--config` + MUR-owned cwd) and never touches the agent's paths.
//!
//! Only definitive answers (supported / unsupported) are cached, per binary
//! path and exact `lang` string, for the life of the server process. A probe
//! that times out, is killed, or exits with anything else is reported and
//! retried next call — a flaky spawn must not pin a language as unsupported.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::process::Command;

/// Upper bound for one probe; the caller also caps it at the search timeout.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Placeholder pattern; empty stdin means it never matches anything.
const PROBE_PATTERN: &str = "x";
/// Probe exit codes (phase 0, item 5). 0 cannot happen on empty stdin but is
/// still a "supported" answer if it does.
const PROBE_SUPPORTED: &[i32] = &[0, 1];
const PROBE_UNSUPPORTED: i32 = 2;
/// Keep the cached message short; it is quoted back to the agent.
const MAX_REASON_BYTES: usize = 512;

type Key = (PathBuf, String);
static CACHE: LazyLock<Mutex<HashMap<Key, Result<(), String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn probe_argv(sgconfig: &Path, lang: &str) -> Vec<OsString> {
    let mut cfg = OsString::from("--config=");
    cfg.push(sgconfig);
    vec![
        "run".into(),
        cfg,
        format!("--pattern={PROBE_PATTERN}").into(),
        format!("--lang={lang}").into(),
        "--stdin".into(),
    ]
}

fn cached(key: &Key) -> Option<Result<(), String>> {
    CACHE.lock().ok()?.get(key).cloned()
}

fn remember(key: Key, v: Result<(), String>) {
    if let Ok(mut c) = CACHE.lock() {
        c.insert(key, v);
    }
}

/// `Ok` if the pinned binary supports `lang`; otherwise an error naming it.
pub async fn ensure_lang(
    bin: &Path,
    cwd: &Path,
    sgconfig: &Path,
    lang: &str,
    timeout: Duration,
) -> Result<(), String> {
    let key = (bin.to_path_buf(), lang.to_owned());
    if let Some(v) = cached(&key) {
        return v;
    }
    let child = Command::new(bin)
        .args(probe_argv(sgconfig, lang))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to start ast-grep language probe: {e}"))?;
    // On timeout the future (and the child) is dropped ⇒ kill_on_drop.
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("ast-grep language probe for '{lang}' timed out"))?
        .map_err(|e| format!("ast-grep language probe failed: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let (reason, _) = super::run::truncate_utf8(stderr.trim(), MAX_REASON_BYTES);
    let verdict = match out.status.code() {
        Some(c) if PROBE_SUPPORTED.contains(&c) => Ok(()),
        Some(PROBE_UNSUPPORTED) => Err(format!(
            "unsupported lang '{lang}' for ast-grep {}: {reason}",
            super::AST_GREP_PINNED_VERSION
        )),
        // Not a definitive answer: report, do not cache.
        Some(c) => {
            return Err(format!(
                "ast-grep language probe for '{lang}' exited {c}: {reason}"
            ));
        }
        None => {
            return Err(format!(
                "ast-grep language probe for '{lang}' was terminated by a signal"
            ));
        }
    };
    remember(key, verdict.clone());
    verdict
}

#[cfg(test)]
#[path = "ast_grep_lang_tests.rs"]
mod tests;
