//! `build_tools`: concurrent MCP tool discovery → (defs, executor map).
//!
//! Call once at agent start to enumerate all non-denied tools across bash
//! and all configured MCP servers.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future;
use mur_common::agent::{McpServerEntry, ToolPolicy, ToolRule, resolve_tool_policy};

use super::fs_policy::SessionCwd;
use super::naming::{sanitize_server, wire_name};
use super::{ToolExecutor, mcp::McpToolExecutor};
use crate::llm::ToolDef;
use crate::mcp::pool::McpPool;

/// What every MCP-backed executor is built against: the shared pool and the
/// agent's session directory, which fan-out tools (`mcp::CWD_ROUTED_TOOLS`)
/// are routed to when the model names no target (#1607).
pub struct McpBackend {
    pub pool: Arc<McpPool>,
    /// `None` in contexts with no session (tests, headless probes).
    pub session_cwd: Option<SessionCwd>,
    /// Where to record each server's discovery outcome
    /// (`mur_common::agent::MCP_STATUS_FILE`) so `mur agent doctor` can report
    /// a server whose tools never loaded. `None` records nothing.
    pub status_file: Option<std::path::PathBuf>,
}

impl McpBackend {
    /// A pool with no session: tools are discovered and called, nothing is
    /// routed.
    pub fn unrouted(pool: Arc<McpPool>) -> Self {
        Self {
            pool,
            session_cwd: None,
            status_file: None,
        }
    }
}

/// Discover bash + all MCP tools, apply policy filter, return (defs, map).
///
/// - `bash`: optional `(def, executor)` pair for the bash tool. Pass `None`
///   if bash is already registered elsewhere.
/// - `servers`: list of MCP server entries to probe.
/// - `rules`: per-tool policy rules from `Entitlements.tools`.
/// - `mcp`: the shared `McpPool` plus the session directory every MCP
///   executor is built against.
pub async fn build_tools(
    bash: Option<(ToolDef, Arc<dyn ToolExecutor>)>,
    read_file: Option<(ToolDef, Arc<dyn ToolExecutor>)>,
    write_file: Option<(ToolDef, Arc<dyn ToolExecutor>)>,
    edit_file: Option<(ToolDef, Arc<dyn ToolExecutor>)>,
    servers: &[McpServerEntry],
    rules: &[ToolRule],
    mcp: McpBackend,
) -> (Vec<ToolDef>, HashMap<String, Arc<dyn ToolExecutor>>) {
    let McpBackend {
        pool,
        session_cwd,
        status_file,
    } = mcp;
    let mut defs: Vec<ToolDef> = Vec::new();
    let mut map: HashMap<String, Arc<dyn ToolExecutor>> = HashMap::new();

    if let Some((def, exec)) = bash
        && resolve_tool_policy(rules, "bash") != ToolPolicy::Deny
    {
        defs.push(def);
        map.insert("bash".to_string(), exec);
    }

    if let Some((def, exec)) = read_file
        && resolve_tool_policy(rules, "read_file") != ToolPolicy::Deny
    {
        defs.push(def);
        map.insert("read_file".to_string(), exec);
    }

    if let Some((def, exec)) = write_file
        && resolve_tool_policy(rules, "write_file") != ToolPolicy::Deny
    {
        defs.push(def);
        map.insert("write_file".to_string(), exec);
    }

    if let Some((def, exec)) = edit_file
        && resolve_tool_policy(rules, "edit_file") != ToolPolicy::Deny
    {
        defs.push(def);
        map.insert("edit_file".to_string(), exec);
    }

    // Built-in no-op suggest_replies. Always in the executor map (so a call can
    // execute); per-turn streaming gating happens in task_runner. Respect an
    // explicit Deny rule.
    if resolve_tool_policy(rules, crate::tools::suggest::SUGGEST_REPLIES) != ToolPolicy::Deny {
        let exec: Arc<dyn ToolExecutor> = Arc::new(crate::tools::suggest::SuggestRepliesTool);
        defs.push(exec.def());
        map.insert(crate::tools::suggest::SUGGEST_REPLIES.to_string(), exec);
    }
    // Built-in `propose`: same registration and gating as suggest_replies;
    // its executor vets the args (mur_common::proposal::vet).
    if resolve_tool_policy(rules, mur_common::proposal::PROPOSE_TOOL) != ToolPolicy::Deny {
        let exec: Arc<dyn ToolExecutor> = Arc::new(crate::tools::propose::ProposeTool);
        defs.push(exec.def());
        map.insert(mur_common::proposal::PROPOSE_TOOL.to_string(), exec);
    }

    let discovery_futs: Vec<_> = servers
        .iter()
        .map(|entry| {
            let pool = pool.clone();
            let name = entry.name.clone();
            let timeout_secs = entry.timeout_secs;
            let kind = entry.kind;
            // A serena entry is pinned to its `project:`; its results get the
            // base-checkout note while a turn works in a track of that repo.
            let serena_project = (kind == Some(mur_common::agent::McpServerKind::Serena))
                .then(|| entry.project.clone())
                .flatten();
            async move {
                let sanitized = sanitize_server(&name);
                match pool.list_tools(&name).await {
                    Ok(tools) => {
                        let tools = crate::mcp::serena::admit_tools(kind, tools);
                        Ok((name, sanitized, tools, timeout_secs, serena_project))
                    }
                    Err(e) => {
                        tracing::warn!(server = %name, "mcp tools/list failed: {e:#}");
                        Err((name, format!("{e:#}")))
                    }
                }
            }
        })
        .collect();

    let discovered = future::join_all(discovery_futs).await;

    if let Some(path) = &status_file {
        record_status(path, &discovered);
    }

    // Every name a server listed, before `deny` rules drop any: the case
    // lint must see a denied `Run` beside a live `run`, or it calls the
    // (working) deny on `Run` a typo of `run`.
    let mut listed_names: Vec<String> = Vec::new();
    for item in discovered.into_iter().flatten() {
        let (server, sanitized, tools, timeout_secs, serena_project) = item;
        listed_names.extend(tools.iter().map(|t| wire_name(&sanitized, &t.name)));
        let listed = ListedServer {
            server: &server,
            timeout_secs,
            serena_project,
        };
        for exec in server_executors(&listed, tools, rules, &pool, session_cwd.as_ref()) {
            defs.push(exec.def());
            map.insert(exec.name().to_string(), exec);
        }
    }

    warn_dead_rules(rules, &map, listed_names);

    (defs, map)
}

/// #2147: log every rule whose pattern differs from a registered tool only by
/// case — matching is exact, so it does nothing (a deny/ask fails open).
/// Tools registered elsewhere (bash_wait, recall, …) count via the builtins;
/// MCP tools count by `listed` (pre-deny), so a denied tool is still known.
fn warn_dead_rules(
    rules: &[ToolRule],
    map: &HashMap<String, Arc<dyn ToolExecutor>>,
    listed: Vec<String>,
) {
    let mut known = mur_common::agent::KnownTools::names(map.keys().map(String::as_str));
    known.names.extend(listed);
    known.names.extend(
        mur_common::agent::BUILTIN_TOOL_NAMES
            .iter()
            .map(|s| s.to_string()),
    );
    for d in mur_common::agent::dead_rules(rules, &known) {
        tracing::warn!(pattern = %d.pattern, suggestion = %d.suggestion, "{}", d.message());
    }
}

/// What a server's executors are built from besides its tool list.
pub(crate) struct ListedServer<'a> {
    pub server: &'a str,
    pub timeout_secs: Option<u32>,
    /// A serena entry's fixed `project:`; `None` for every other server.
    pub serena_project: Option<std::path::PathBuf>,
}

impl<'a> ListedServer<'a> {
    pub fn of(entry: &'a McpServerEntry) -> Self {
        Self {
            server: &entry.name,
            timeout_secs: entry.timeout_secs,
            serena_project: (entry.kind == Some(mur_common::agent::McpServerKind::Serena))
                .then(|| entry.project.clone())
                .flatten(),
        }
    }
}

/// One executor per tool `server` listed, minus the ones a `ToolRule` denies.
/// Shared by boot discovery and the per-turn refresh after a hot reload, so a
/// re-listed server is filtered by exactly the rules boot applied.
pub(crate) fn server_executors(
    listed: &ListedServer<'_>,
    tools: Vec<crate::protocol::mcp_client::ToolInfo>,
    rules: &[ToolRule],
    pool: &Arc<McpPool>,
    session_cwd: Option<&SessionCwd>,
) -> Vec<Arc<dyn ToolExecutor>> {
    let sanitized = sanitize_server(listed.server);
    let timeout = listed
        .timeout_secs
        .map(|s| std::time::Duration::from_secs(s as u64))
        .unwrap_or(super::mcp::MCP_TOOL_TIMEOUT);
    tools
        .into_iter()
        .filter_map(|t| {
            let wname = wire_name(&sanitized, &t.name);
            if resolve_tool_policy(rules, &wname) == ToolPolicy::Deny {
                return None;
            }
            let def = ToolDef {
                name: wname.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            };
            Some(Arc::new(McpToolExecutor {
                wire_name: wname,
                server: listed.server.to_string(),
                tool: t.name,
                def,
                pool: pool.clone(),
                timeout,
                session_cwd: session_cwd.cloned(),
                serena_project: listed.serena_project.clone(),
            }) as Arc<dyn ToolExecutor>)
        })
        .collect()
}

type Discovered = Result<
    (
        String,
        String,
        Vec<crate::protocol::mcp_client::ToolInfo>,
        Option<u32>,
        Option<std::path::PathBuf>,
    ),
    (String, String),
>;

/// Best-effort write of [`mur_common::agent::McpStatus`]. A failed write only
/// costs `doctor` its MCP line, so it is logged and never fails the boot.
fn record_status(path: &std::path::Path, discovered: &[Discovered]) {
    use mur_common::agent::{McpServerStatus, McpStatus};
    let servers = discovered
        .iter()
        .map(|d| match d {
            Ok((name, _, tools, _, _)) => McpServerStatus {
                name: name.clone(),
                tools: tools.len(),
                error: None,
            },
            Err((name, err)) => McpServerStatus {
                name: name.clone(),
                tools: 0,
                error: Some(err.clone()),
            },
        })
        .collect();
    let status = McpStatus {
        pid: std::process::id(),
        servers,
    };
    if let Err(e) = status.save(path) {
        tracing::warn!(path = %path.display(), "could not record MCP status: {e}");
    }
}

/// Register `bash_wait`/`bash_kill` iff `bash` itself was registered — the
/// three share one gate (spec D6/D11). Call after `build_tools`.
pub fn attach_bash_control(
    map: &mut HashMap<String, Arc<dyn ToolExecutor>>,
    controls: Vec<Arc<dyn ToolExecutor>>,
) {
    if !map.contains_key("bash") {
        return;
    }
    for tool in controls {
        map.insert(tool.name().to_string(), tool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::sandbox::SandboxPolicy;

    /// Always-registered built-ins: `suggest_replies` and `propose`.
    const BUILTINS: usize = 2;

    /// A server whose `tools/list` fails used to vanish with only a stderr
    /// warning; it must now be on disk for `mur agent doctor`.
    #[tokio::test]
    async fn failed_discovery_is_recorded_in_status_file() {
        let entry = mur_common::agent::McpServerEntry {
            name: "broken".into(),
            command: "/definitely/not/a/real/mcp-binary".into(),
            ..Default::default()
        };
        let pool = McpPool::new(vec![entry.clone()], SandboxPolicy::default(), None);
        let dir = tempfile::tempdir().unwrap();
        let status_path = dir.path().join(mur_common::agent::MCP_STATUS_FILE);
        let backend = McpBackend {
            status_file: Some(status_path.clone()),
            ..McpBackend::unrouted(pool)
        };
        let (defs, _) = build_tools(None, None, None, None, &[entry], &[], backend).await;
        assert_eq!(defs.len(), BUILTINS);

        let status = mur_common::agent::McpStatus::load(&status_path).expect("status written");
        assert_eq!(status.pid, std::process::id());
        let failed: Vec<_> = status.failed().map(|s| s.name.as_str()).collect();
        assert_eq!(failed, ["broken"]);
    }

    #[tokio::test]
    async fn no_servers_empty_result() {
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) =
            build_tools(None, None, None, None, &[], &[], McpBackend::unrouted(pool)).await;
        // the chrome built-ins (suggest_replies, propose) are always registered.
        assert_eq!(defs.len(), BUILTINS);
        assert!(map.contains_key("suggest_replies"));
        assert!(map.contains_key("propose"));
    }

    #[tokio::test]
    async fn bash_tool_included_when_allowed() {
        use crate::tools::bash::BashTool;
        let bash_exec: Arc<dyn ToolExecutor> = Arc::new(BashTool {
            working_dir: std::path::PathBuf::from("/tmp"),
            session_cwd: crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            agent: None,
            write_grants: Vec::new(),
            secrets: None,
            scratch_dir: None,
            jobs: crate::tools::bash_jobs::JobTable::new(),
            trash: None,
        });
        let bash_def = bash_exec.def();
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) = build_tools(
            Some((bash_def, bash_exec)),
            None,
            None,
            None,
            &[],
            &[],
            McpBackend::unrouted(pool),
        )
        .await;
        // bash + the chrome built-ins
        assert_eq!(defs.len(), BUILTINS + 1);
        assert!(map.contains_key("bash"));
    }

    #[tokio::test]
    async fn suggest_replies_is_registered() {
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) =
            build_tools(None, None, None, None, &[], &[], McpBackend::unrouted(pool)).await;
        assert!(map.contains_key("suggest_replies"));
        assert!(defs.iter().any(|d| d.name == "suggest_replies"));
    }

    #[tokio::test]
    async fn bash_tool_excluded_when_denied() {
        use crate::tools::bash::BashTool;
        let bash_exec: Arc<dyn ToolExecutor> = Arc::new(BashTool {
            working_dir: std::path::PathBuf::from("/tmp"),
            session_cwd: crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            agent: None,
            write_grants: Vec::new(),
            secrets: None,
            scratch_dir: None,
            jobs: crate::tools::bash_jobs::JobTable::new(),
            trash: None,
        });
        let bash_def = bash_exec.def();
        let rules = vec![ToolRule {
            pattern: "bash".to_string(),
            policy: ToolPolicy::Deny,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }];
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) = build_tools(
            Some((bash_def, bash_exec)),
            None,
            None,
            None,
            &[],
            &rules,
            McpBackend::unrouted(pool),
        )
        .await;
        // bash is denied; the chrome built-ins are still registered.
        assert_eq!(defs.len(), BUILTINS);
        assert!(!map.contains_key("bash"));
        assert!(map.contains_key("suggest_replies"));
    }

    #[tokio::test]
    async fn read_file_tool_included_when_allowed() {
        use crate::tools::read_file::ReadFileTool;
        let read_file_exec: Arc<dyn ToolExecutor> = Arc::new(ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            mur_common::agent::FilesystemEntitlement::default(),
        ));
        let read_file_def = read_file_exec.def();
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) = build_tools(
            None,
            Some((read_file_def, read_file_exec)),
            None,
            None,
            &[],
            &[],
            McpBackend::unrouted(pool),
        )
        .await;
        // read_file + the chrome built-ins
        assert_eq!(defs.len(), BUILTINS + 1);
        assert!(map.contains_key("read_file"));
    }

    #[tokio::test]
    async fn read_file_tool_excluded_when_denied() {
        use crate::tools::read_file::ReadFileTool;
        let read_file_exec: Arc<dyn ToolExecutor> = Arc::new(ReadFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            mur_common::agent::FilesystemEntitlement::default(),
        ));
        let read_file_def = read_file_exec.def();
        let rules = vec![ToolRule {
            pattern: "read_file".to_string(),
            policy: ToolPolicy::Deny,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }];
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) = build_tools(
            None,
            Some((read_file_def, read_file_exec)),
            None,
            None,
            &[],
            &rules,
            McpBackend::unrouted(pool),
        )
        .await;
        // read_file is denied; the chrome built-ins are still registered.
        assert_eq!(defs.len(), BUILTINS);
        assert!(!map.contains_key("read_file"));
        assert!(map.contains_key("suggest_replies"));
    }

    #[tokio::test]
    async fn write_file_tool_included_when_allowed() {
        use crate::tools::write_file::WriteFileTool;
        let write_file_exec: Arc<dyn ToolExecutor> = Arc::new(WriteFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            mur_common::agent::FilesystemEntitlement::default(),
        ));
        let write_file_def = write_file_exec.def();
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) = build_tools(
            None,
            None,
            Some((write_file_def, write_file_exec)),
            None,
            &[],
            &[],
            McpBackend::unrouted(pool),
        )
        .await;
        // write_file + the chrome built-ins
        assert_eq!(defs.len(), BUILTINS + 1);
        assert!(map.contains_key("write_file"));
    }

    #[tokio::test]
    async fn write_file_tool_excluded_when_denied() {
        use crate::tools::write_file::WriteFileTool;
        let write_file_exec: Arc<dyn ToolExecutor> = Arc::new(WriteFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            mur_common::agent::FilesystemEntitlement::default(),
        ));
        let write_file_def = write_file_exec.def();
        let rules = vec![ToolRule {
            pattern: "write_file".to_string(),
            policy: ToolPolicy::Deny,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }];
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) = build_tools(
            None,
            None,
            Some((write_file_def, write_file_exec)),
            None,
            &[],
            &rules,
            McpBackend::unrouted(pool),
        )
        .await;
        // write_file is denied; the chrome built-ins are still registered.
        assert_eq!(defs.len(), BUILTINS);
        assert!(!map.contains_key("write_file"));
        assert!(map.contains_key("suggest_replies"));
    }

    #[tokio::test]
    async fn edit_file_tool_included_when_allowed() {
        use crate::tools::edit_file::EditFileTool;
        let edit_file_exec: Arc<dyn ToolExecutor> = Arc::new(EditFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            mur_common::agent::FilesystemEntitlement::default(),
        ));
        let edit_file_def = edit_file_exec.def();
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) = build_tools(
            None,
            None,
            None,
            Some((edit_file_def, edit_file_exec)),
            &[],
            &[],
            McpBackend::unrouted(pool),
        )
        .await;
        // edit_file + the chrome built-ins
        assert_eq!(defs.len(), BUILTINS + 1);
        assert!(map.contains_key("edit_file"));
    }

    #[tokio::test]
    async fn edit_file_tool_excluded_when_denied() {
        use crate::tools::edit_file::EditFileTool;
        let edit_file_exec: Arc<dyn ToolExecutor> = Arc::new(EditFileTool::new_for_test(
            crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            mur_common::agent::FilesystemEntitlement::default(),
        ));
        let edit_file_def = edit_file_exec.def();
        let rules = vec![ToolRule {
            pattern: "edit_file".to_string(),
            policy: ToolPolicy::Deny,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }];
        let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
        let (defs, map) = build_tools(
            None,
            None,
            None,
            Some((edit_file_def, edit_file_exec)),
            &[],
            &rules,
            McpBackend::unrouted(pool),
        )
        .await;
        // edit_file is denied; the chrome built-ins are still registered.
        assert_eq!(defs.len(), BUILTINS);
        assert!(!map.contains_key("edit_file"));
        assert!(map.contains_key("suggest_replies"));
    }

    /// Test 16: denying `bash` registers none of the three; allowing it
    /// registers all three.
    #[tokio::test]
    async fn bash_control_tools_follow_bash_registration() {
        use crate::tools::bash::BashTool;
        let mk = || {
            Arc::new(BashTool::new(
                std::path::PathBuf::from("/tmp"),
                crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            ))
        };
        let deny = vec![ToolRule {
            pattern: "bash".into(),
            policy: ToolPolicy::Deny,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }];
        for (rules, expect) in [(deny, false), (vec![], true)] {
            let bash = mk();
            let exec: Arc<dyn ToolExecutor> = bash.clone();
            let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
            let (_defs, mut map) = build_tools(
                Some((bash.def(), exec)),
                None,
                None,
                None,
                &[],
                &rules,
                McpBackend::unrouted(pool),
            )
            .await;
            attach_bash_control(&mut map, bash.control_tools());
            assert_eq!(map.contains_key("bash"), expect);
            assert_eq!(map.contains_key("bash_wait"), expect);
            assert_eq!(map.contains_key("bash_kill"), expect);
        }
    }
}
