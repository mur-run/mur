//! MCP client — subprocess stdio JSON-RPC 2.0.

use crate::sandbox::SandboxPolicy;
use mur_common::agent::McpServerEntry;
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;

pub struct McpClient {
    /// Raw std child — used for kill/wait in `shutdown`.
    child: Mutex<std::process::Child>,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<Lines<BufReader<ChildStdout>>>,
    /// Piped stderr, available once via `take_stderr()`. Callers should drain
    /// this in a background task to prevent the child's stderr buffer filling.
    stderr: Mutex<Option<std::process::ChildStderr>>,
    next_id: Mutex<u64>,
    pub server_name: String,
}

#[derive(Debug)]
pub struct InitializeInfo {
    pub server_name: String,
    pub server_version: String,
    pub protocol_version: String,
}

#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("mcp server closed stdout")]
    StreamClosed,
    #[error("mcp error: {0}")]
    Server(String),
}

impl McpClient {
    /// Spawn an MCP server subprocess under the given sandbox policy.
    ///
    /// Uses `sandbox::child::spawn_sandboxed` so that on Linux and macOS the
    /// child inherits the supervisor's Landlock / seccomp / SBPL restrictions
    /// (B1 Tasks 2–3).  Sync stdin / stdout handles from `std::process::Child`
    /// are promoted to async via `ChildStdin::from_std` / `ChildStdout::from_std`.
    pub async fn spawn(entry: &McpServerEntry, policy: &SandboxPolicy) -> Result<Self, McpError> {
        let mut std_cmd = std::process::Command::new(&entry.command);
        std_cmd
            .args(&entry.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::sandbox::child::spawn_sandboxed(std_cmd, policy)?;

        let raw_stdin = child.stdin.take().ok_or(McpError::StreamClosed)?;
        let raw_stdout = child.stdout.take().ok_or(McpError::StreamClosed)?;
        let raw_stderr = child.stderr.take();
        let stdin = ChildStdin::from_std(raw_stdin)?;
        let stdout = ChildStdout::from_std(raw_stdout)?;

        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout).lines()),
            stderr: Mutex::new(raw_stderr),
            next_id: Mutex::new(1),
            server_name: entry.name.clone(),
        })
    }

    /// Take the child's stderr handle for background draining. Returns `None`
    /// if already taken or if the child was spawned without a piped stderr.
    pub async fn take_stderr(&self) -> Option<std::process::ChildStderr> {
        self.stderr.lock().await.take()
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = {
            let mut g = self.next_id.lock().await;
            let v = *g;
            *g += 1;
            v
        };
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let line = format!("{req}\n");
        {
            let mut s = self.stdin.lock().await;
            s.write_all(line.as_bytes()).await?;
            s.flush().await?;
        }
        let mut stdout = self.stdout.lock().await;
        loop {
            let next = stdout.next_line().await?;
            let line = next.ok_or(McpError::StreamClosed)?;
            let v: Value = serde_json::from_str(&line)?;
            if v.get("id") == Some(&json!(id)) {
                if let Some(err) = v.get("error") {
                    return Err(McpError::Server(err.to_string()));
                }
                return Ok(v.get("result").cloned().unwrap_or(json!(null)));
            }
            // notifications (no matching id) — ignore for now
        }
    }

    pub async fn initialize(&mut self) -> Result<InitializeInfo, McpError> {
        let res = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {"name": "mur-agent-runtime", "version": "0.1.0"}
                }),
            )
            .await?;
        Ok(InitializeInfo {
            server_name: res["serverInfo"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            server_version: res["serverInfo"]["version"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            protocol_version: res["protocolVersion"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, McpError> {
        let res = self.request("tools/list", json!({})).await?;
        let tools = res["tools"].as_array().cloned().unwrap_or_default();
        Ok(tools
            .into_iter()
            .map(|t| ToolInfo {
                name: t["name"].as_str().unwrap_or_default().to_string(),
                description: t["description"].as_str().unwrap_or_default().to_string(),
                input_schema: t["inputSchema"].clone(),
            })
            .collect())
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }

    pub async fn shutdown(self) {
        let mut child = self.child.into_inner();
        let _ = child.kill();
        // Reap the child off the async thread so we don't block the runtime.
        let _ = tokio::task::spawn_blocking(move || child.wait()).await;
    }
}
