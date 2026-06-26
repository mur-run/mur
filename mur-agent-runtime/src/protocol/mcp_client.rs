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
    /// HTTP / network transport failure (Streamable HTTP transport).
    #[error("transport: {0}")]
    Transport(String),
    /// JSON-RPC error object returned by the server.
    #[error("rpc error: {0}")]
    Rpc(String),
    /// Unexpected response shape from the server.
    #[error("protocol: {0}")]
    Protocol(String),
    /// Server returned HTTP 401 Unauthorized.
    #[error("unauthorized")]
    Unauthorized,
}

impl InitializeInfo {
    /// Build from a JSON-RPC `result` value returned by the `initialize` method.
    /// Returns `Err(reason)` if the shape is completely wrong (empty strings are
    /// tolerated as the stdio path does via `unwrap_or_default`).
    pub fn from_result(result: &Value) -> Result<Self, String> {
        Ok(Self {
            server_name: result["serverInfo"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            server_version: result["serverInfo"]["version"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            protocol_version: result["protocolVersion"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }
}

impl ToolInfo {
    /// Build a list of `ToolInfo` from a JSON-RPC `result` value returned by
    /// the `tools/list` method. Missing or malformed `tools` array yields an
    /// empty list (mirrors the stdio path which uses `unwrap_or_default`).
    pub fn list_from_result(result: &Value) -> Result<Vec<Self>, String> {
        let tools = result["tools"].as_array().cloned().unwrap_or_default();
        Ok(tools
            .into_iter()
            .map(|t| ToolInfo {
                name: t["name"].as_str().unwrap_or_default().to_string(),
                description: t["description"].as_str().unwrap_or_default().to_string(),
                input_schema: t["inputSchema"].clone(),
            })
            .collect())
    }
}

impl McpClient {
    /// Spawn an MCP server subprocess under the given sandbox policy.
    ///
    /// Uses `sandbox::child::spawn_sandboxed` so that on Linux and macOS the
    /// child inherits the supervisor's Landlock / seccomp / SBPL restrictions
    /// (B1 Tasks 2–3).  Sync stdin / stdout handles from `std::process::Child`
    /// are promoted to async via `ChildStdin::from_std` / `ChildStdout::from_std`.
    pub async fn spawn(
        entry: &McpServerEntry,
        policy: &SandboxPolicy,
        proxy: Option<&crate::sandbox::egress_proxy::EgressProxyHandle>,
    ) -> Result<Self, McpError> {
        let mut std_cmd = std::process::Command::new(&entry.command);
        std_cmd
            .args(&entry.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Per-server egress: set HTTP_PROXY ONLY on this child's env (never the
        // runtime's) when the server declares a Restricted policy. Empty
        // otherwise ⇒ byte-for-byte the previous behavior.
        for (k, v) in proxy_env_for(entry, proxy) {
            std_cmd.env(k, v);
        }
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

    /// Send a JSON-RPC *notification* (no `id`, no response expected).
    /// Used for the MCP lifecycle `notifications/initialized` handshake step.
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let req = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let line = format!("{req}\n");
        let mut s = self.stdin.lock().await;
        s.write_all(line.as_bytes()).await?;
        s.flush().await?;
        Ok(())
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
        // MCP lifecycle step 3: the client MUST send `notifications/initialized`
        // after the `initialize` response, otherwise spec-compliant servers
        // reject every subsequent request (`tools/list`, `tools/call`) with
        // "Not initialized". Universal — required by all MCP servers, not just ours.
        self.notify("notifications/initialized", json!({})).await?;
        InitializeInfo::from_result(&res).map_err(McpError::Server)
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, McpError> {
        let res = self.request("tools/list", json!({})).await?;
        ToolInfo::list_from_result(&res).map_err(McpError::Server)
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

/// Env vars that scope a policied MCP child to its allowlist via the egress
/// proxy. Returns empty (no change vs today) unless the server is `Restricted`
/// AND a proxy is available. NEVER set these on the runtime's own environment —
/// only on the child `Command` (Global Constraint: env isolation, so the
/// agent's own LLM/cc-proxy path is never affected).
pub fn proxy_env_for(
    entry: &McpServerEntry,
    proxy: Option<&crate::sandbox::egress_proxy::EgressProxyHandle>,
) -> Vec<(String, String)> {
    let (Some(net), Some(proxy)) = (entry.network.as_ref(), proxy) else {
        return vec![];
    };
    if net.mode != mur_common::agent::McpNetMode::Restricted {
        return vec![];
    }
    let token = proxy.register(net.allow_hosts.clone());
    let url = format!("http://{token}:x@{}", proxy.addr);
    let no_proxy = "127.0.0.1,localhost,::1".to_string();
    vec![
        ("HTTP_PROXY".into(), url.clone()),
        ("HTTPS_PROXY".into(), url.clone()),
        ("http_proxy".into(), url.clone()),
        ("https_proxy".into(), url),
        ("NO_PROXY".into(), no_proxy.clone()),
        ("no_proxy".into(), no_proxy),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{McpNetMode, McpServerNetwork};
    use std::collections::HashMap;

    #[test]
    fn proxy_env_only_for_restricted_servers() {
        let base = McpServerEntry {
            name: "x".into(),
            command: "npx".into(),
            ..Default::default()
        };

        // No policy → no env (byte-for-byte today).
        assert!(proxy_env_for(&base, None).is_empty());

        // Restricted but no proxy handle → still empty (defensive).
        let mut restricted = base.clone();
        restricted.network = Some(McpServerNetwork {
            mode: McpNetMode::Restricted,
            allow_hosts: vec!["example.com".into()],
        });
        assert!(proxy_env_for(&restricted, None).is_empty());

        // Restricted + proxy → HTTP_PROXY/HTTPS_PROXY/NO_PROXY set; loopback in NO_PROXY.
        let handle = crate::sandbox::egress_proxy::EgressProxyHandle::for_test(
            "127.0.0.1:9".parse().unwrap(),
        );
        let env: HashMap<_, _> = proxy_env_for(&restricted, Some(&handle))
            .into_iter()
            .collect();
        assert!(env.get("HTTP_PROXY").unwrap().contains("@127.0.0.1:9"));
        assert!(env.get("HTTPS_PROXY").unwrap().contains("@127.0.0.1:9"));
        assert!(env.get("NO_PROXY").unwrap().contains("127.0.0.1"));

        // Inherit/None mode → no env even with a proxy.
        let mut inherit = base.clone();
        inherit.network = Some(McpServerNetwork {
            mode: McpNetMode::Inherit,
            allow_hosts: vec![],
        });
        assert!(proxy_env_for(&inherit, Some(&handle)).is_empty());
    }
}
