//! Pre-fetch the servers serena downloads itself, so setup can smoke them
//! (#1993 proposal 2).
//!
//! Lua, C/C++, Java and Kotlin are not installed by MUR: serena fetches them
//! into `<SERENA_HOME>/language_servers/static/` on first start. Setup used to
//! have nothing on disk to launch for them, so a server the seal refuses to
//! exec only surfaced later, as a serena that would not start.
//!
//! Here setup runs serena's own fetch code ([`SCRIPT`]) with serena's own
//! interpreter and the entry's `SERENA_HOME` — the same code path and the same
//! destination serena uses at start — and gets back the argv serena would
//! launch. That argv is then smoked under the seal like any managed install.
//!
//! The fetch runs unsealed, as setup does: it needs the network and writes
//! the entry's serena home. Only the launch afterwards is sealed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::plan::Lang;
use super::serena_install::{PACKAGE, UV_TOOL_SUBDIR};
use super::smoke::Target;

/// The fetch script, run by serena's interpreter.
const SCRIPT: &str = include_str!("smoke_fetch.py");
const SCRIPT_NAME: &str = "mur_smoke_fetch.py";

/// The download is a few tens of MB from GitHub; this bounds a stalled one.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(300);

/// Lines of the script's stderr quoted back when it dies without a report.
const STDERR_TAIL_LINES: usize = 4;

/// What fetching one language gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// On disk; launch it as `Target::cmd`.
    Ready(Target),
    /// serena could not fetch it here.
    Failed { lang: Lang, detail: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Entry {
    Ok(Vec<String>),
    Err(String),
}

/// The interpreter of the serena tool env in `serena_dir`
/// (`<dir>/uv-tools/serena-agent/bin/python`).
pub fn interpreter(serena_dir: &Path) -> PathBuf {
    let env = serena_dir.join(UV_TOOL_SUBDIR).join(PACKAGE);
    if cfg!(windows) {
        env.join("Scripts").join("python.exe")
    } else {
        env.join("bin").join("python")
    }
}

/// Fetch `langs` into `serena_home` with the serena install in `serena_dir`.
/// Every language gets an answer; a script that dies fails all of them.
pub fn fetch(serena_dir: &Path, serena_home: &Path, langs: &[Lang]) -> Vec<Fetched> {
    fetch_within(serena_dir, serena_home, langs, FETCH_TIMEOUT)
}

/// [`fetch`] with an explicit deadline (tests use a short one).
pub(crate) fn fetch_within(
    serena_dir: &Path,
    serena_home: &Path,
    langs: &[Lang],
    timeout: Duration,
) -> Vec<Fetched> {
    if langs.is_empty() {
        return Vec::new();
    }
    match run_script(&interpreter(serena_dir), serena_home, langs, timeout) {
        Ok(stdout) => parse(&stdout, langs),
        Err(e) => all_failed(langs, &format!("{e:#}")),
    }
}

/// Run [`SCRIPT`] in a scratch root (serena writes a `cache/` there) and
/// return its stdout.
fn run_script(
    python: &Path,
    serena_home: &Path,
    langs: &[Lang],
    timeout: Duration,
) -> Result<String> {
    let scratch = tempfile::tempdir().context("create a scratch dir for the fetch")?;
    let script = scratch.path().join(SCRIPT_NAME);
    std::fs::write(&script, SCRIPT).context("write the fetch script")?;
    let root = scratch.path().join("root");
    std::fs::create_dir(&root).context("create the fetch root")?;

    let mut child = Command::new(python)
        .arg(&script)
        .arg(&root)
        .args(langs.iter().map(|l| l.flag()))
        .current_dir(&root)
        .env("SERENA_HOME", serena_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start serena's interpreter {}", python.display()))?;

    // One reader per pipe: draining them in sequence deadlocks once the
    // child fills the pipe we are not reading yet.
    let stdout = child.stdout.take().context("fetch stdout")?;
    let stderr = child.stderr.take().context("fetch stderr")?;
    let err_reader = std::thread::spawn(move || drain(stderr));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = drain(stdout);
        let err = err_reader.join().unwrap_or_default();
        let _ = tx.send((out, err));
    });
    let Ok((out, err)) = rx.recv_timeout(timeout) else {
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!("the fetch did not finish within {}s", timeout.as_secs());
    };
    let status = child.wait().context("wait for the fetch")?;
    if out.trim().is_empty() {
        anyhow::bail!(
            "serena's fetch exited ({status}) without a report{}",
            tail(&err).map(|t| format!(": {t}")).unwrap_or_default()
        );
    }
    Ok(out)
}

/// Read a pipe to EOF, keeping whatever arrived if it errors.
fn drain(mut pipe: impl Read) -> String {
    let mut buf = String::new();
    let _ = pipe.read_to_string(&mut buf);
    buf
}

/// The script's report, one answer per requested language.
pub(crate) fn parse(stdout: &str, langs: &[Lang]) -> Vec<Fetched> {
    let report = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<std::collections::BTreeMap<String, Entry>>(l).ok());
    let Some(mut report) = report else {
        return all_failed(langs, "serena's fetch printed no report");
    };
    langs
        .iter()
        .map(|&lang| match report.remove(lang.flag()) {
            Some(Entry::Ok(cmd)) if !cmd.is_empty() => Fetched::Ready(Target { lang, cmd }),
            Some(Entry::Ok(_)) => Fetched::Failed {
                lang,
                detail: "serena returned an empty launch command".into(),
            },
            Some(Entry::Err(detail)) => Fetched::Failed { lang, detail },
            None => Fetched::Failed {
                lang,
                detail: "serena's fetch did not report it".into(),
            },
        })
        .collect()
}

fn all_failed(langs: &[Lang], detail: &str) -> Vec<Fetched> {
    langs
        .iter()
        .map(|&lang| Fetched::Failed {
            lang,
            detail: detail.to_owned(),
        })
        .collect()
}

/// The last few non-empty stderr lines, joined.
fn tail(err: &str) -> Option<String> {
    let lines: Vec<&str> = err
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(STDERR_TAIL_LINES);
    (!lines.is_empty()).then(|| lines[start..].join(" | "))
}

#[cfg(all(test, unix))]
#[path = "smoke_fetch_tests.rs"]
mod tests;
