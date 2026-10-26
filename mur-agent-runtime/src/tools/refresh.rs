//! Per-turn MCP tool refresh after a hot reload (spec 2026-10-25 §5, §8).
//!
//! The pool already starts a new client generation when a server's entry
//! changes. What it cannot do is change the tool list the model sees: that was
//! built once at boot. At turn start this re-lists every server whose entry
//! moved since it was last listed, replaces only that server's executors, and
//! re-applies the same `ToolRule`s boot applied.
//!
//! It also writes the turn's notices: tool names added or removed (D8; a
//! schema- or description-only change is logged, not announced), a new
//! config that failed to start, and edits the reload gate refused or that need
//! a restart. Each notice is said once while it holds, not every turn.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use mur_common::agent::{McpServerEntry, ToolRule};
use mur_common::mcp_naming::sanitize_server;

use super::ToolExecutor;
use super::fs_policy::SessionCwd;
use super::registry::{ListedServer, server_executors};
use crate::mcp::pool::McpPool;

/// The `ReloadState` paths and refusals a turn reports, read after the entries
/// so they describe the same reload.
pub struct ReloadView {
    /// MCP server → why its edited entry was refused.
    pub refusals: BTreeMap<String, String>,
    /// `pending_cold_restart` as recorded (every field, not only MCP ones).
    pub cold: Vec<String>,
}

/// What a refresh changed. `tools` is `None` when no server was re-listed.
#[derive(Default)]
pub struct Refreshed {
    pub tools: Option<Vec<Arc<dyn ToolExecutor>>>,
    pub notices: Vec<String>,
    /// A notice said this turn is one only a restart applies, so the reply
    /// carries a restart chip (spec §8). Not set for a refusal or a config
    /// that failed to start: a restart reads the same profile.
    pub restart: bool,
}

/// Label of the restart chip a reply carries with a restart notice.
pub const RESTART_LABEL: &str = "restart to apply the MCP change";

/// A notice that holds until the profile changes.
struct Standing {
    text: String,
    needs_restart: bool,
}

/// One server as last listed.
#[derive(Clone)]
struct Listed {
    entry: McpServerEntry,
    /// Tool names (unprefixed) the model was offered, after `ToolRule`s.
    names: BTreeSet<String>,
    /// Over every offered tool's name, description and schema, so a schema
    /// change with the same names is still noticed (and only logged).
    digest: u64,
}

#[derive(Default)]
struct State {
    listed: HashMap<String, Listed>,
    /// Refusal and restart notices already shown. Replaced every turn by the
    /// ones still true, so a cleared condition can be reported again later.
    said: HashSet<String>,
}

pub struct McpRefresh {
    pool: Arc<McpPool>,
    session_cwd: Option<SessionCwd>,
    agent: String,
    /// Held for the whole refresh: two turns starting together list once.
    state: tokio::sync::Mutex<State>,
    /// Bound on one server's re-list (spawn, `initialize`, `tools/list`).
    /// `None`: the server's own tool-call timeout, so a server configured as
    /// slow is given the same patience here.
    list_timeout: Option<Duration>,
}

impl McpRefresh {
    /// `boot_tools` is the tool set boot built from the pool's boot entries.
    pub fn new(
        pool: Arc<McpPool>,
        session_cwd: Option<SessionCwd>,
        agent: String,
        boot_tools: &[Arc<dyn ToolExecutor>],
    ) -> Self {
        let listed = pool
            .servers()
            .filter_map(|s| {
                let entry = pool.current_entry(s)?;
                let execs: Vec<_> = of_server(boot_tools, s).cloned().collect();
                Some((s.to_string(), listed(entry, &execs)))
            })
            .collect();
        Self {
            pool,
            session_cwd,
            agent,
            state: tokio::sync::Mutex::new(State {
                listed,
                said: HashSet::new(),
            }),
            list_timeout: None,
        }
    }

    /// Bound every re-list by `t` instead of each server's tool-call timeout.
    pub fn with_list_timeout(mut self, t: Duration) -> Self {
        self.list_timeout = Some(t);
        self
    }

    /// Re-list the servers whose entry changed and return the new tool set
    /// plus this turn's notices. `view` is read after the entries.
    pub async fn refresh(
        &self,
        current: &[Arc<dyn ToolExecutor>],
        rules: &[ToolRule],
        view: impl FnOnce() -> ReloadView,
    ) -> Refreshed {
        let mut state = self.state.lock().await;
        let mut servers: Vec<&str> = self.pool.servers().collect();
        servers.sort_unstable();
        let changed: Vec<(String, McpServerEntry)> = servers
            .into_iter()
            .filter_map(|s| {
                let now = self.pool.current_entry(s)?;
                let same = state.listed.get(s).is_some_and(|l| l.entry == now);
                (!same).then(|| (s.to_string(), now))
            })
            .collect();

        let mut out = Refreshed::default();
        let relisted = !changed.is_empty();
        let mut tools: Vec<Arc<dyn ToolExecutor>> = current.to_vec();
        for (server, entry) in changed {
            let previous = state.listed.get(&server).cloned();
            let bound = self.list_timeout.unwrap_or_else(|| {
                entry
                    .timeout_secs
                    .map_or(super::mcp::MCP_TOOL_TIMEOUT, |s| {
                        Duration::from_secs(s.into())
                    })
            });
            let listing = match tokio::time::timeout(bound, self.pool.list_tools(&server)).await {
                Ok(r) => r.map_err(anyhow::Error::from),
                Err(_) => Err(anyhow::anyhow!(
                    "no answer within {}s (timed out)",
                    bound.as_secs_f32()
                )),
            };
            match listing {
                Ok(listed_tools) => {
                    let listed_tools = crate::mcp::serena::admit_tools(entry.kind, listed_tools);
                    let fresh = server_executors(
                        &ListedServer::of(&entry),
                        listed_tools,
                        rules,
                        &self.pool,
                        self.session_cwd.as_ref(),
                    );
                    tools.retain(|t| !belongs_to(t.name(), &server));
                    tools.extend(fresh.iter().cloned());
                    let now = listed(entry, &fresh);
                    if let Some(prev) = &previous {
                        out.notices.extend(name_diff(&server, prev, &now));
                    }
                    state.listed.insert(server, now);
                }
                Err(e) => {
                    tracing::warn!(server = %server, "re-list after MCP reload failed: {e:#}");
                    out.notices.push(format!(
                        "MCP `{server}` new config failed to start: {e:#}. Its tools fail \
                         until profile.yaml is fixed or the agent restarts."
                    ));
                    // Keep the names the model was last offered, but record the
                    // entry so the failure is reported once, not every turn.
                    let (names, digest) = previous.map(|p| (p.names, p.digest)).unwrap_or_default();
                    state.listed.insert(
                        server,
                        Listed {
                            entry,
                            names,
                            digest,
                        },
                    );
                }
            }
        }
        if relisted {
            out.tools = Some(tools);
        }

        let mut servers: Vec<String> = state.listed.keys().cloned().collect();
        servers.sort_unstable();
        let standing = standing_notices(&view(), &servers, &self.agent);
        for n in &standing {
            if !state.said.contains(&n.text) {
                out.notices.push(n.text.clone());
                out.restart |= n.needs_restart;
            }
        }
        state.said = standing.into_iter().map(|n| n.text).collect();
        out
    }
}

/// The executors registered for `server` (`mcp__<sanitized>__<tool>`).
fn of_server<'a>(
    tools: &'a [Arc<dyn ToolExecutor>],
    server: &'a str,
) -> impl Iterator<Item = &'a Arc<dyn ToolExecutor>> {
    tools.iter().filter(move |t| belongs_to(t.name(), server))
}

fn prefix(server: &str) -> String {
    format!("mcp__{}__", sanitize_server(server))
}

fn belongs_to(wire: &str, server: &str) -> bool {
    wire.starts_with(&prefix(server))
}

fn listed(entry: McpServerEntry, execs: &[Arc<dyn ToolExecutor>]) -> Listed {
    use std::hash::{Hash, Hasher};
    let p = prefix(&entry.name);
    let mut defs: Vec<_> = execs.iter().map(|t| t.def()).collect();
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for d in &defs {
        d.name.hash(&mut h);
        d.description.hash(&mut h);
        d.input_schema.to_string().hash(&mut h);
    }
    Listed {
        names: defs
            .iter()
            .map(|d| d.name.strip_prefix(&p).unwrap_or(&d.name).to_string())
            .collect(),
        digest: h.finish(),
        entry,
    }
}

/// D8: a notice only when names were added or removed; a schema- or
/// description-only change is logged.
fn name_diff(server: &str, before: &Listed, after: &Listed) -> Option<String> {
    let added: Vec<_> = after.names.difference(&before.names).collect();
    let removed: Vec<_> = before.names.difference(&after.names).collect();
    if added.is_empty() && removed.is_empty() {
        if before.digest != after.digest {
            tracing::info!(server, "MCP tool schemas or descriptions changed on reload");
        }
        return None;
    }
    let list = |v: &[&String]| {
        v.iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!("added {}", list(&added)));
    }
    if !removed.is_empty() {
        parts.push(format!("removed {}", list(&removed)));
    }
    Some(format!("MCP `{server}` reloaded: {}", parts.join("; ")))
}

/// Notices that stay true until the profile changes: refused edits and MCP
/// edits that need a restart. `servers` is sorted.
fn standing_notices(view: &ReloadView, servers: &[String], agent: &str) -> Vec<Standing> {
    let restart = format!("`mur agent restart {agent}`");
    let mut out: Vec<Standing> = view
        .refusals
        .iter()
        .map(|(s, why)| Standing {
            text: format!("MCP `{s}` reload refused: {why}; still running the previous config."),
            needs_restart: false,
        })
        .collect();
    if view.cold.iter().any(|p| p == "mcp_servers") {
        out.push(Standing {
            text: format!("MCP servers were added or removed; that needs a restart: {restart}."),
            needs_restart: true,
        });
    }
    for s in servers {
        let head = format!("mcp_servers.{s}.");
        let fields: Vec<&str> = view
            .cold
            .iter()
            .filter_map(|p| p.strip_prefix(&head))
            .collect();
        if !fields.is_empty() {
            out.push(Standing {
                text: format!(
                    "MCP `{s}` edit to {} needs a restart; still running the previous config. \
                     Run {restart}.",
                    fields.join(", ")
                ),
                needs_restart: true,
            });
        }
    }
    out
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod tests;
