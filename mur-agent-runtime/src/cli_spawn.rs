//! Run one turn inside a spawned coding CLI.
//!
//! The CLI owns the agentic loop; MUR owns the tools, which reach it through
//! the shim named in the per-turn `--mcp-config`. Nothing here executes a
//! tool, and nothing here reads the user's own CLI configuration: the spawn
//! runs against this backend's private home.
//!
//! See `docs/superpowers/specs/2026-09-16-cli-spawn-backends-design.md`.

use mur_common::cli_backend::{CliBackend, ISOLATION_FLAGS, ensure_home, mcp_config_json};
use std::path::{Path, PathBuf};

pub struct SpawnRequest<'a> {
    pub backend: &'a CliBackend,
    pub mur_home: &'a Path,
    pub shim_bin: &'a str,
    pub socket: &'a Path,
    pub task_id: &'a str,
    pub prompt: &'a str,
}

/// The assistant text from a `stream-json` transcript.
///
/// Concatenates every assistant text block in order. Tool traffic is not
/// included: those calls already ran through MUR's handler and were recorded
/// there, so repeating them here would double-count a turn's history.
pub fn reply_from_stream_json(lines: &str) -> String {
    let mut out = String::new();
    for line in lines.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if v["type"] != "assistant" {
            continue;
        }
        let Some(blocks) = v["message"]["content"].as_array() else {
            continue;
        };
        for b in blocks {
            if b["type"] == "text"
                && let Some(t) = b["text"].as_str()
            {
                out.push_str(t);
            }
        }
    }
    out
}

/// The private home for this backend, or `None` to use the user's own.
///
/// The single place the decision lives. `claude` returns `None` today: tool
/// isolation comes from the flags, so a private home would buy only
/// lifecycle uniformity across backends, and uniformity is not worth
/// charging the user a second login for. `agy` has no home variable at all,
/// so its answer is different and belongs here rather than in an `if` at the
/// spawn site.
///
/// Changing this decision is this function plus nothing else.
fn claude_home(
    mur_home: &Path,
    backend: &CliBackend,
) -> anyhow::Result<Option<(&'static str, PathBuf)>> {
    if backend.key == "claude" {
        return Ok(None);
    }
    Ok(Some(ensure_home(mur_home, backend)?))
}

pub async fn run_turn(req: SpawnRequest<'_>) -> anyhow::Result<String> {
    let home = claude_home(req.mur_home, req.backend)?;
    // The MCP config is ours either way; it never goes near the user's home.
    let cfg_dir = req.mur_home.join("cli-homes").join(req.backend.key);
    std::fs::create_dir_all(&cfg_dir)?;
    let cfg_path = cfg_dir.join("mur-mcp.json");
    std::fs::write(
        &cfg_path,
        serde_json::to_vec_pretty(&mcp_config_json(req.shim_bin, req.socket, req.task_id))?,
    )?;

    let mut cmd = tokio::process::Command::new(req.backend.binary);
    cmd.args(req.backend.headless_invocation)
        .args(req.backend.stream_flags)
        .args(ISOLATION_FLAGS)
        .arg("--mcp-config")
        .arg(&cfg_path)
        .stdin(std::process::Stdio::piped());
    // Injected only when there is one — the whole decision is `claude_home`.
    if let Some((var, path)) = &home {
        cmd.env(var, path);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        stdin.write_all(req.prompt.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        // Dropped here: the CLI reads its prompt to EOF, so holding this open
        // would leave it waiting for input that is never coming.
    }

    let out = child.wait_with_output().await?;
    if !out.status.success() {
        anyhow::bail!(
            "{} exited {}: {}",
            req.backend.binary,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(reply_from_stream_json(&String::from_utf8_lossy(
        &out.stdout,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `claude --output-format stream-json` transcript shape.
    const TRANSCRIPT: &str = r#"{"type":"system","subtype":"init","tools":[]}
{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__mur__bash"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":" and goodbye"}]}}
{"type":"result","subtype":"success"}"#;

    #[test]
    fn the_reply_is_the_assistant_text_in_order() {
        assert_eq!(reply_from_stream_json(TRANSCRIPT), "Hello and goodbye");
    }

    #[test]
    fn tool_traffic_is_not_part_of_the_reply() {
        // Those calls already ran through MUR's handler and were recorded
        // there. Repeating them would double-count the turn's history.
        assert!(!reply_from_stream_json(TRANSCRIPT).contains("bash"));
    }

    #[test]
    fn a_malformed_line_does_not_discard_the_rest() {
        let s = format!("not json\n{TRANSCRIPT}");
        assert_eq!(reply_from_stream_json(&s), "Hello and goodbye");
    }

    #[test]
    fn an_empty_transcript_is_an_empty_reply_not_a_panic() {
        assert_eq!(reply_from_stream_json(""), "");
    }
}
