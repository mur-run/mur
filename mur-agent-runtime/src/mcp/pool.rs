//! Lazy-cached MCP subprocess pool. One `McpPool` per agent; servers are
//! spawned on first use and reused for the agent's lifetime.

use std::collections::HashMap;
use std::sync::Arc;

use mur_common::agent::McpServerEntry;
use tokio::sync::Mutex;

use crate::protocol::mcp_client::{McpClient, McpError, ToolInfo};
use crate::sandbox::SandboxPolicy;

pub struct McpPool {
    entries: HashMap<String, McpServerEntry>,
    policy: SandboxPolicy,
    clients: Mutex<HashMap<String, Arc<Mutex<McpClient>>>>,
}

impl McpPool {
    pub fn new(entries: Vec<McpServerEntry>, policy: SandboxPolicy) -> Arc<Self> {
        let map = entries.into_iter().map(|e| (e.name.clone(), e)).collect();
        Arc::new(Self {
            entries: map,
            policy,
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
        let mut client = McpClient::spawn(entry, &self.policy).await?;
        client.initialize().await?;

        // Drain child stderr in a background thread so the pipe never fills.
        if let Some(stderr) = client.take_stderr().await {
            let server_name = server.to_string();
            tokio::task::spawn_blocking(move || {
                use std::io::{BufRead, BufReader};
                for line in BufReader::new(stderr).lines() {
                    match line {
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
            // Only shut down if we hold the last Arc reference.
            if let Ok(mutex) = Arc::try_unwrap(arc) {
                mutex.into_inner().shutdown().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicy;

    #[tokio::test]
    async fn unknown_server_returns_error() {
        let pool = McpPool::new(vec![], SandboxPolicy::default());
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
