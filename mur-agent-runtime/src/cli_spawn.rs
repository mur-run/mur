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
    /// Where each assistant block goes as it arrives. `None` for an
    /// unattended turn, where nobody is reading.
    pub deltas: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
}

/// The assistant text from a `stream-json` transcript.
///
/// Concatenates every assistant text block in order. Tool traffic is not
/// included: those calls already ran through MUR's handler and were recorded
/// there, so repeating them here would double-count a turn's history.
pub fn reply_from_stream_json(lines: &str) -> String {
    let mut out = String::new();
    for line in lines.lines() {
        for block in blocks_in(line) {
            if !block.thinking {
                out.push_str(&block.text);
            }
        }
    }
    out
}

/// One assistant block from the transcript.
///
/// `thinking` blocks are streamed so a long thinking phase is visible, but
/// they are not part of the reply — the same split the gateway path makes.
pub(crate) struct Block {
    pub text: String,
    pub thinking: bool,
}

/// The assistant blocks in a single `stream-json` line, in order.
///
/// The primitive both forms are built from: `reply_from_stream_json` folds it
/// over a whole transcript, `drain_stream_json` calls it per line as the line
/// arrives. Having one parser is the point — a second one would drift, and
/// the drift would be a reply that differs from what the user watched.
pub(crate) fn blocks_in(line: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return out;
    };
    if v["type"] != "assistant" {
        return out;
    }
    let Some(blocks) = v["message"]["content"].as_array() else {
        return out;
    };
    for b in blocks {
        // Tool traffic is excluded from both: those calls already ran through
        // MUR's handler and were recorded there.
        let thinking = match b["type"].as_str() {
            Some("text") => false,
            Some("thinking") => true,
            _ => continue,
        };
        let key = if thinking { "thinking" } else { "text" };
        if let Some(t) = b[key].as_str()
            && !t.is_empty()
        {
            out.push(Block {
                text: t.to_string(),
                thinking,
            });
        }
    }
    out
}

/// Read `stream-json` to EOF, emitting each assistant block as it arrives and
/// returning the reply.
///
/// A closed sink does NOT stop the read. The client disconnecting must not
/// truncate what this turn is recorded as having said, and leaving stdout
/// undrained would block the CLI on a full pipe.
pub(crate) async fn drain_stream_json<R>(
    reader: R,
    mut deltas: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
) -> std::io::Result<String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut reply = String::new();
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        for block in blocks_in(&line) {
            if !block.thinking {
                reply.push_str(&block.text);
            }
            if let Some(tx) = &deltas {
                let d = crate::llm::StreamDelta {
                    text: block.text,
                    thinking: block.thinking,
                };
                // Full sink: wait, so a slow reader slows the turn rather
                // than losing it. Closed sink: stop sending, keep reading.
                if tx.send(d).await.is_err() {
                    deltas = None;
                }
            }
        }
    }
    Ok(reply)
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

    // stderr is drained on its own task: a CLI that writes more than a pipe
    // buffer of diagnostics would otherwise block forever while we read stdout.
    let mut stderr_buf = Vec::new();
    let mut stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        if let Some(e) = &mut stderr {
            let _ = e.read_to_end(&mut stderr_buf).await;
        }
        stderr_buf
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let reply = drain_stream_json(tokio::io::BufReader::new(stdout), req.deltas).await?;

    let status = child.wait().await?;
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    if !status.success() {
        anyhow::bail!(
            "{} exited {}: {}",
            req.backend.binary,
            status,
            String::from_utf8_lossy(&stderr_bytes).trim()
        );
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `claude --output-format stream-json` transcript shape.
    const TRANSCRIPT: &str = r#"{"type":"system","subtype":"init","tools":[]}
{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}
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

    async fn drain(t: &str) -> (String, Vec<(String, bool)>) {
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let reply = drain_stream_json(std::io::Cursor::new(t.to_string()), Some(tx))
            .await
            .expect("drain");
        let mut got = Vec::new();
        while let Ok(d) = rx.try_recv() {
            got.push((d.text, d.thinking));
        }
        (reply, got)
    }

    #[tokio::test]
    async fn each_block_is_its_own_delta_not_one_at_the_end() {
        // The whole point of the change. One delta carrying the joined reply
        // would pass a naive "did anything stream" check while the user still
        // waited for the turn to finish.
        let (reply, deltas) = drain(TRANSCRIPT).await;
        assert_eq!(
            deltas,
            vec![
                ("hmm".to_string(), true),
                ("Hello".to_string(), false),
                (" and goodbye".to_string(), false),
            ]
        );
        assert_eq!(reply, "Hello and goodbye");
    }

    #[tokio::test]
    async fn what_was_streamed_is_what_is_returned() {
        // Two parsers would drift, and the drift would be a recorded reply
        // that differs from what the user watched arrive.
        let (reply, deltas) = drain(TRANSCRIPT).await;
        let watched: String = deltas
            .iter()
            .filter(|(_, thinking)| !thinking)
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(watched, reply);
        assert_eq!(reply, reply_from_stream_json(TRANSCRIPT));
    }

    #[tokio::test]
    async fn thinking_is_streamed_but_is_not_the_reply() {
        let (reply, deltas) = drain(TRANSCRIPT).await;
        assert!(deltas.iter().any(|(_, thinking)| *thinking));
        assert!(!reply.contains("hmm"));
    }

    #[tokio::test]
    async fn a_closed_sink_does_not_truncate_the_reply() {
        // A client that disconnects mid-turn must not change what the turn is
        // recorded as having said — and leaving stdout undrained would block
        // the CLI on a full pipe.
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        drop(rx);
        let reply = drain_stream_json(std::io::Cursor::new(TRANSCRIPT.to_string()), Some(tx))
            .await
            .expect("drain survives a dead sink");
        assert_eq!(reply, "Hello and goodbye");
    }

    #[tokio::test]
    async fn an_unattended_turn_needs_no_sink() {
        let reply = drain_stream_json(std::io::Cursor::new(TRANSCRIPT.to_string()), None)
            .await
            .expect("drain");
        assert_eq!(reply, "Hello and goodbye");
    }
}
