//! Lazy-cached MCP subprocess pool. One `McpPool` per agent; servers are
//! spawned on first use and reused for the agent's lifetime.

use std::collections::HashMap;
use std::sync::Arc;

use mur_common::agent::McpServerEntry;
use tokio::sync::Mutex;

use crate::protocol::mcp_client::{McpClient, McpError, ToolInfo};
use crate::sandbox::SandboxPolicy;
use crate::sandbox::egress_proxy::EgressProxyHandle;

pub struct McpPool {
    entries: HashMap<String, McpServerEntry>,
    policy: SandboxPolicy,
    /// Shared egress proxy for servers with a `Restricted` network policy.
    /// `None` when no server on this agent declares one (the common case).
    proxy: Option<EgressProxyHandle>,
    clients: Mutex<HashMap<String, Arc<Mutex<McpClient>>>>,
}

impl McpPool {
    pub fn new(
        entries: Vec<McpServerEntry>,
        policy: SandboxPolicy,
        proxy: Option<EgressProxyHandle>,
    ) -> Arc<Self> {
        let map = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
        Arc::new(Self {
            entries: map,
            policy,
            proxy,
            clients: Mutex::new(HashMap::new()),
        })
    }

    /// Return (or lazily spawn) the client for `server`.
    pub async fn client(&self, server: &str) -> Result<Arc<Mutex<McpClient>>, McpError> {
        let mut guard = self.clients.lock().await;
        if let Some(c) = guard.get(server) {
            return Ok(c.clone());
        }

        let entry = self.entries.get(server).ok_or_else(|| {
            McpError::Server(format!("no MCP server named `{server}` on this agent"))
        })?;
        let mut client = McpClient::connect(entry, &self.policy, self.proxy.as_ref()).await?;
        client.initialize().await?;

        // Drain child stderr in a background thread so the pipe never fills.
        if let Some(stderr) = client.take_stderr().await {
            let server_name = server.to_string();
            tokio::task::spawn_blocking(move || {
                use std::io::{BufRead, BufReader};
                for line in BufReader::new(stderr).lines() {
                    match line {
                        Ok(l) if is_noteworthy_stderr(&l) => {
                            tracing::warn!(server = %server_name, "mcp stderr: {l}")
                        }
                        Ok(l) => tracing::debug!(server = %server_name, "mcp stderr: {l}"),
                        Err(_) => break,
                    }
                }
            });
        }

        let shared = Arc::new(Mutex::new(client));
        guard.insert(server.to_string(), shared.clone());
        Ok(shared)
    }

    /// List tools exposed by `server`, spawning it if needed.
    pub async fn list_tools(&self, server: &str) -> Result<Vec<ToolInfo>, McpError> {
        let c = self.client(server).await?;
        c.lock().await.list_tools().await
    }

    /// Shut down all warm clients gracefully.
    pub async fn shutdown(&self) {
        let mut guard = self.clients.lock().await;
        let clients: Vec<_> = guard.drain().map(|(_, v)| v).collect();
        drop(guard);
        for arc in clients {
            // Only shut down if we hold the last Arc reference. When another
            // holder (e.g. an in-flight tool call) still has a clone, we
            // can't kill+wait the child here without racing that call; the
            // eventual last drop is still reap-safe because
            // `StdioMcpClient`'s `Drop` impl kills and waits synchronously,
            // so no zombie survives regardless of which reference is last.
            if let Ok(mutex) = Arc::try_unwrap(arc) {
                mutex.into_inner().shutdown().await;
            }
        }
    }
}

/// Whether a child MCP server's stderr line should surface at `warn!` instead
/// of `debug!`. Routine chatter stays at debug so a chatty server cannot flood
/// the agent log — but a child's own WARN/ERROR is often the operator's ONLY
/// signal that the server degraded rather than failed outright: a secret ref
/// that silently fell back to a default leaves a working-but-wrong config with
/// no other trace.
// ponytail: substring match on the level marker — covers the tracing /
// env_logger style output MUR's own servers emit. Widen the match rather than
// parsing per-server log formats.
fn is_noteworthy_stderr(line: &str) -> bool {
    line.contains("WARN") || line.contains("ERROR")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicy;

    #[test]
    fn child_warnings_surface_above_debug_but_chatter_does_not() {
        // The line that went missing in practice: the research gateway warns
        // that a keychain-backed brave_api_key_ref did not resolve, then
        // silently falls back to a keyless search path. Logged at debug!, it
        // never reached the agent log and the degraded config looked healthy.
        assert!(is_noteworthy_stderr(
            "2026-08-03T09:01:20Z  WARN mur_research_gateway: brave_api_key_ref did not resolve"
        ));
        assert!(is_noteworthy_stderr("ERROR failed to open index"));
        // Routine chatter stays at debug — a per-call audit line must not
        // promote itself into the operator's warning stream.
        assert!(!is_noteworthy_stderr(
            r#"INFO research_gateway_audit: {"verb":"search","outcome":"ok"}"#
        ));
        assert!(!is_noteworthy_stderr("starting up"));
    }

    #[tokio::test]
    async fn unknown_server_returns_error() {
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        match pool.client("nonexistent").await {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("nonexistent"),
                    "error should name the server: {msg}"
                );
            }
            Ok(_) => panic!("expected error for unknown server"),
        }
    }
}
