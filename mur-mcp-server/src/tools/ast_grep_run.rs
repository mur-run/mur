//! `ast_grep_search` execution: spawn, bounded stream parse, exit mapping.
//!
//! Phase 0 item 10: one long line can make ast-grep emit output quadratic in
//! the line length (5k matches on one line = 127 MB). So stdout is read
//! through a byte budget (`max_output_bytes`), never collected whole; each
//! match's `text` / `lines` is cut to `max_match_bytes`; and the child is
//! killed (and reaped) the moment any cap trips. A cut is always reported
//! as `truncated` with a reason — never silent.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use mur_common::config::AstGrepLimits;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// ast-grep exit codes (phase 0, item 4).
const EXIT_MATCH: i32 = 0;
const EXIT_NO_MATCH: i32 = 1;
const EXIT_BAD_ARGS: i32 = 2;
const EXIT_CONFIG: i32 = 79;

/// stderr is diagnostics, not data; keep enough to show every warning.
const MAX_STDERR_BYTES: u64 = 64 * 1024;
/// After the child exits or is killed, how long stderr may stay open. A
/// grandchild holding the pipe must not stall the tool past its deadline.
const STDERR_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    MaxResults,
    OutputBytes,
    Timeout,
}

impl TruncatedBy {
    fn as_str(self) -> &'static str {
        match self {
            Self::MaxResults => "max_results",
            Self::OutputBytes => "max_output_bytes",
            Self::Timeout => "timeout",
        }
    }
}

/// Cut `s` to at most `max` bytes on a char boundary. `true` = it was cut.
pub fn truncate_utf8(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

fn cut(s: &str, max: usize, flag: &mut bool) -> Value {
    let (t, c) = truncate_utf8(s, max);
    *flag |= c;
    Value::String(t.to_owned())
}

/// One `--json=stream` record → the agent-facing match. Lines and columns
/// become 1-based (ast-grep emits 0-based). `None` = not a match record.
pub fn shape_match(raw: &Value, max_match_bytes: usize) -> Option<Value> {
    let file = raw.get("file")?.as_str()?;
    let start = raw.pointer("/range/start")?;
    let end = raw.pointer("/range/end")?;
    let pos = |p: &Value, k: &str| p.get(k).and_then(Value::as_u64).map(|n| n + 1);
    let mut truncated = false;
    let mut m = Map::new();
    m.insert("file".into(), file.into());
    m.insert("line".into(), pos(start, "line")?.into());
    m.insert("column".into(), pos(start, "column")?.into());
    m.insert("end_line".into(), pos(end, "line")?.into());
    m.insert("end_column".into(), pos(end, "column")?.into());
    let text = raw.get("text").and_then(Value::as_str).unwrap_or("");
    m.insert("text".into(), cut(text, max_match_bytes, &mut truncated));
    if let Some(l) = raw.get("lines").and_then(Value::as_str) {
        m.insert("lines".into(), cut(l, max_match_bytes, &mut truncated));
    }
    if let Some(lang) = raw.get("language").and_then(Value::as_str) {
        m.insert("language".into(), lang.into());
    }
    let mut vars = Map::new();
    if let Some(single) = raw
        .pointer("/metaVariables/single")
        .and_then(Value::as_object)
    {
        for (k, v) in single {
            let t = v.get("text").and_then(Value::as_str).unwrap_or("");
            vars.insert(k.clone(), cut(t, max_match_bytes, &mut truncated));
        }
    }
    if let Some(multi) = raw
        .pointer("/metaVariables/multi")
        .and_then(Value::as_object)
    {
        for (k, v) in multi {
            let items = v.as_array().map(Vec::as_slice).unwrap_or_default();
            let texts = items
                .iter()
                .map(|i| {
                    cut(
                        i.get("text").and_then(Value::as_str).unwrap_or(""),
                        max_match_bytes,
                        &mut truncated,
                    )
                })
                .collect();
            vars.insert(k.clone(), Value::Array(texts));
        }
    }
    if !vars.is_empty() {
        m.insert("meta_variables".into(), Value::Object(vars));
    }
    if truncated {
        m.insert("truncated".into(), true.into());
    }
    Some(Value::Object(m))
}

/// Exit status of a run MUR did not cut short → `Ok` (0/1) or an error the
/// agent can act on. stderr is quoted in every error.
pub fn map_exit(code: Option<i32>, stderr: &str) -> Result<(), String> {
    let detail = stderr.trim();
    match code {
        Some(EXIT_MATCH | EXIT_NO_MATCH) => Ok(()),
        Some(EXIT_BAD_ARGS) => Err(format!(
            "ast-grep rejected the arguments (exit 2): {detail}"
        )),
        Some(EXIT_CONFIG) => Err(format!(
            "ast-grep exited 79 (configuration error). MUR passes its own empty \
             sgconfig, so this means a config MUR did not supply was loaded; the \
             search was not run. stderr: {detail}"
        )),
        Some(c) => Err(format!("ast-grep failed (exit {c}): {detail}")),
        None => Err(format!("ast-grep was terminated by a signal: {detail}")),
    }
}

fn warnings(stderr: &str) -> Vec<Value> {
    stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| Value::String(l.to_owned()))
        .collect()
}

/// Run one search. `argv` comes from `build_argv`; `cwd` is the MUR-owned
/// isolation dir.
pub async fn run(
    bin: &Path,
    argv: &[std::ffi::OsString],
    cwd: &Path,
    limits: &AstGrepLimits,
) -> Result<Value, String> {
    let mut child = Command::new(bin)
        .args(argv)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to start ast-grep: {e}"))?;
    let stdout = child.stdout.take().ok_or("ast-grep stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("ast-grep stderr unavailable")?;
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        // Drain past the cap so the child never blocks on a full pipe.
        let mut s = stderr;
        let _ = (&mut s).take(MAX_STDERR_BYTES).read_to_end(&mut buf).await;
        let _ = tokio::io::copy(&mut s, &mut tokio::io::sink()).await;
        String::from_utf8_lossy(&buf).into_owned()
    });

    // +1 so "exactly at the cap" and "over the cap" are distinguishable.
    let mut reader = BufReader::new(stdout.take(limits.max_output_bytes + 1));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(limits.timeout_secs);
    let max_match = usize::try_from(limits.max_match_bytes).unwrap_or(usize::MAX);
    let mut matches = Vec::new();
    let mut consumed: u64 = 0;
    let mut skipped_lines = 0u32;
    let mut cut_by = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = match tokio::time::timeout_at(deadline, reader.read_until(b'\n', &mut line)).await {
            Err(_) => {
                cut_by = Some(TruncatedBy::Timeout);
                break;
            }
            Ok(Err(e)) => return Err(format!("reading ast-grep output failed: {e}")),
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
        };
        consumed += n as u64;
        if consumed > limits.max_output_bytes {
            // The partial record at the cap is dropped, not half-parsed.
            cut_by = Some(TruncatedBy::OutputBytes);
            break;
        }
        if matches.len() as u64 >= u64::from(limits.max_results) {
            cut_by = Some(TruncatedBy::MaxResults);
            break;
        }
        match serde_json::from_slice::<Value>(&line)
            .ok()
            .and_then(|v| shape_match(&v, max_match))
        {
            Some(m) => matches.push(m),
            None => skipped_lines += 1,
        }
    }

    let status = match cut_by {
        Some(_) => None,
        // stdout closed, but the deadline still bounds the exit.
        None => match tokio::time::timeout_at(deadline, child.wait()).await {
            Ok(st) => Some(st.map_err(|e| format!("waiting for ast-grep failed: {e}"))?),
            Err(_) => {
                cut_by = Some(TruncatedBy::Timeout);
                None
            }
        },
    };
    if status.is_none() {
        // kill = start_kill + wait: the child is reaped, never a zombie.
        let _ = child.kill().await;
    }
    let mut stderr_task = stderr_task;
    let (stderr, stderr_lost) = match tokio::time::timeout(STDERR_GRACE, &mut stderr_task).await {
        Ok(r) => (r.unwrap_or_default(), false),
        Err(_) => {
            stderr_task.abort();
            (String::new(), true)
        }
    };
    if let Some(st) = status {
        map_exit(st.code(), &stderr)?;
    }

    let mut warn = warnings(&stderr);
    if stderr_lost {
        warn.push("ast-grep stderr stayed open after exit; warnings unavailable".into());
    }
    if skipped_lines > 0 {
        warn.push(format!("{skipped_lines} unparseable output line(s) skipped").into());
    }
    let mut out = json!({
        "count": matches.len(),
        "matches": matches,
        "truncated": cut_by.is_some(),
        "warnings": warn,
        "limits": {
            "max_results": limits.max_results,
            "context_lines": limits.context_lines,
            "timeout_secs": limits.timeout_secs,
            "max_output_bytes": limits.max_output_bytes,
            "max_match_bytes": limits.max_match_bytes,
        },
    });
    if let Some(c) = cut_by {
        out["truncated_by"] = c.as_str().into();
    }
    Ok(out)
}

/// The `ast_grep_search` tool entry point.
pub async fn call(mur_home: &Path, arguments: &Value) -> Result<Value, String> {
    let bin = super::resolve_binary(mur_home).ok_or_else(|| {
        format!(
            "ast-grep {} is not installed at {}",
            super::AST_GREP_PINNED_VERSION,
            super::binary_path(mur_home).display()
        )
    })?;
    let args = super::parse_args(arguments)?;
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join(super::CONFIG_FILE));
    let limits = cfg.search.ast_grep.resolve(args.max_results, args.context);
    let iso = super::ensure_isolation(mur_home)
        .map_err(|e| format!("ast-grep isolation dir unavailable: {e}"))?;
    let argv = super::build_argv(&args, &iso.sgconfig, limits.context_lines);
    run(&bin, &argv, &iso.cwd, &limits).await
}

#[cfg(test)]
#[path = "ast_grep_run_tests.rs"]
mod tests;
