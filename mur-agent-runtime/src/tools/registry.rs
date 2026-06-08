//! `build_tools`: concurrent MCP tool discovery → (defs, executor map).
//!
//! Call once at agent start to enumerate all non-denied tools across bash
//! and all configured MCP servers.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future;
use mur_common::agent::{McpServerEntry, ToolPolicy, ToolRule, resolve_tool_policy};

use super::{ToolExecutor, mcp::McpToolExecutor};
use super::naming::{sanitize_server, wire_name};
use crate::llm::ToolDef;
use crate::mcp::pool::McpPool;

/// Discover bash + all MCP tools, apply policy filter, return (defs, map).
///
/// - `bash`: optional `(def, executor)` pair for the bash tool. Pass `None`
///   if bash is already registered elsewhere.
/// - `servers`: list of MCP server entries to probe.
/// - `rules`: per-tool policy rules from `Entitlements.tools`.
/// - `pool`: shared `McpPool` for the agent.
pub async fn build_tools(
    bash: Option<(ToolDef, Arc<dyn ToolExecutor>)>,
    servers: &[McpServerEntry],
    rules: &[ToolRule],
    pool: Arc<McpPool>,
) -> (Vec<ToolDef>, HashMap<String, Arc<dyn ToolExecutor>>) {
    let mut defs: Vec<ToolDef> = Vec::new();
    let mut map: HashMap<String, Arc<dyn ToolExecutor>> = HashMap::new();

    if let Some((def, exec)) = bash {
        if resolve_tool_policy(rules, "bash") != ToolPolicy::Deny {
            defs.push(def);
            map.insert("bash".to_string(), exec);
        }
    }

    let discovery_futs: Vec<_> = servers
        .iter()
        .map(|entry| {
            let pool = pool.clone();
            let name = entry.name.clone();
            async move {
                let sanitized = sanitize_server(&name);
                match pool.list_tools(&name).await {
                    Ok(tools) => Some((name, sanitized, tools)),
                    Err(e) => {
                        tracing::warn!(server = %name, "mcp tools/list failed: {e}");
                        None
                    }
                }
            }
        })
        .collect();

    let discovered = future::join_all(discovery_futs).await;

    for item in discovered.into_iter().flatten() {
        let (server, sanitized, tools) = item;
        for t in tools {
            let wname = wire_name(&sanitized, &t.name);
            if resolve_tool_policy(rules, &wname) == ToolPolicy::Deny {
                continue;
            }
            let def = ToolDef {
                name: wname.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            };
            defs.push(def.clone());
            let exec: Arc<dyn ToolExecutor> = Arc::new(McpToolExecutor {
                wire_name: wname.clone(),
                server: server.clone(),
                tool: t.name.clone(),
                def,
                pool: pool.clone(),
                timeout: super::mcp::MCP_TOOL_TIMEOUT,
            });
            map.insert(wname, exec);
        }
    }

    (defs, map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxPolicy;

    #[tokio::test]
    async fn no_servers_empty_result() {
        let pool = McpPool::new(vec![], SandboxPolicy::default());
        let (defs, map) = build_tools(None, &[], &[], pool).await;
        assert!(defs.is_empty());
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn bash_tool_included_when_allowed() {
        use crate::tools::bash::BashTool;
        let bash_exec: Arc<dyn ToolExecutor> = Arc::new(BashTool { working_dir: std::path::PathBuf::from("/tmp") });
        let bash_def = bash_exec.def();
        let pool = McpPool::new(vec![], SandboxPolicy::default());
        let (defs, map) = build_tools(Some((bash_def, bash_exec)), &[], &[], pool).await;
        assert_eq!(defs.len(), 1);
        assert!(map.contains_key("bash"));
    }

    #[tokio::test]
    async fn bash_tool_excluded_when_denied() {
        use crate::tools::bash::BashTool;
        let bash_exec: Arc<dyn ToolExecutor> = Arc::new(BashTool { working_dir: std::path::PathBuf::from("/tmp") });
        let bash_def = bash_exec.def();
        let rules = vec![ToolRule {
            pattern: "bash".to_string(),
            policy: ToolPolicy::Deny,
        }];
        let pool = McpPool::new(vec![], SandboxPolicy::default());
        let (defs, map) = build_tools(Some((bash_def, bash_exec)), &[], &rules, pool).await;
        assert!(defs.is_empty());
        assert!(!map.contains_key("bash"));
    }
}
