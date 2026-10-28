//! Which real runtime tools each B0 `pre_tool_use` rule gates (#2181).
//!
//! B0's rules were written against dotted names (`fs.write`,
//! `network.http_get`) the runtime never registers, so after #2175 wired the
//! hook in, Rules 1, 2 and 4 still never fired. Every name here is the tool's
//! own `const`, so renaming a tool renames its gate; `classify_builtin` is a
//! total match over every built-in, and `tool_map_tests` fails when a tool is
//! added without being classified.

use std::path::{Path, PathBuf};

use crate::hooks::ToolCall;
use crate::tools::fs_policy::{HardWriteRefusal, for_file_tools, hard_write_refusal};

/// The leaf of an MCP wire name (`mcp__<server>__<leaf>`), or the name itself
/// for a built-in. Rules match the leaf so the operator's server name cannot
/// move a tool out from under its gate — the same reasoning as
/// `mur_common::hitl::builtin_tier::browser_tier`.
pub(super) fn leaf(name: &str) -> &str {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.split_once("__").map_or(rest, |(_, l)| l),
        None => name,
    }
}

/// Whether a built-in tool has an effect outside the model's own context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Effect {
    /// Changes state the user can see, spends, or starts work elsewhere.
    SideEffect,
    /// Reads, waits, or offers a choice the user still has to make.
    ReadOnly,
}

/// Every built-in the runtime registers, by its own name constant. `None` =
/// not a built-in (an MCP tool, a test double, or a legacy dotted name).
pub(super) fn classify_builtin(name: &str) -> Option<Effect> {
    use crate::tools::{
        bash::BASH, bash_control, edit_file::EDIT_FILE, fleet_run::FLEET_RUN, git_push,
        open_item::OPEN_ITEM, read_file::READ_FILE, recall::RECALL, remember::REMEMBER,
        remind::REMIND, suggest::SUGGEST_REPLIES, write_file::WRITE_FILE,
    };
    use mur_common::proposal::PROPOSE_TOOL;
    Some(match name {
        BASH
        | bash_control::BASH_KILL
        | WRITE_FILE
        | EDIT_FILE
        | REMEMBER
        | REMIND
        | OPEN_ITEM
        | FLEET_RUN
        | git_push::GIT_PUSH_REQUEST
        | git_push::GIT_PUSH_CANCEL => Effect::SideEffect,
        READ_FILE
        | RECALL
        | SUGGEST_REPLIES
        | PROPOSE_TOOL
        | git_push::GIT_PUSH_STATUS
        | bash_control::BASH_WAIT => Effect::ReadOnly,
        _ => return None,
    })
}

/// MCP tools (matched on the leaf) that dispatch agents and spend.
const DISPATCH_LEAVES: &[&str] = &["parallel_jobs"];

/// Rule 4's question: may this call act on the world? Built-ins answer from
/// [`classify_builtin`]; MCP dispatch tools and acting browser tools are
/// side effects; anything else falls to the name heuristic, applied to the
/// leaf so `mcp__slack__send_message` reads as `send_message`.
pub(super) fn is_side_effect_tool(name: &str) -> bool {
    if let Some(effect) = classify_builtin(name) {
        return effect == Effect::SideEffect;
    }
    let leaf = leaf(name);
    if DISPATCH_LEAVES.contains(&leaf) {
        return true;
    }
    if let Some(tier) = mur_common::hitl::builtin_tier::browser_tier(name) {
        return tier > mur_common::hitl::RiskTier::Read;
    }
    name_looks_side_effecting(name) || name_looks_side_effecting(leaf)
}

/// The original M3.8 heuristic, for tools that are neither built-ins nor
/// known MCP leaves.
fn name_looks_side_effecting(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("delete")
        || n.ends_with("delete")
        || n.starts_with("spawn")
        || n.ends_with("spawn")
        || n.contains(".send")
        || n.starts_with("send")
        || n.starts_with("egress")
        || n.starts_with("network.")
        || n.contains(".write")
        || n.contains(".publish")
}

/// Rule 2's question: does this call reach a URL, and which? `None` = not a
/// network tool. `Some(None)` = a network tool whose target B0 cannot read,
/// which Rule 2 treats as an unknown host.
///
/// MCP leaves only count when they carry `url`: `browser_tabs` opens a URL
/// only with `action: new`, and gating its `list` would ask about no host.
/// `fleet_run` egresses through deep-research but names no URL; its own
/// `--grant-egress` consent covers it.
pub(super) fn outbound_url(call: &ToolCall) -> Option<Option<&str>> {
    /// MCP leaves whose `url` field is the request target.
    const URL_LEAVES: &[&str] = &["browser_navigate", "browser_tabs", "fetch"];
    /// The M7.3 names. No built-in registers them; kept so a hook consumer
    /// that does (and the B0 corpus) keeps its gate.
    const LEGACY_NET_TOOLS: &[&str] = &[
        "network.http_get",
        "network.http_post",
        "network.tcp_connect",
        "network.udp_send",
        "network.dns_resolve",
        "fetch",
    ];
    let name = call.name();
    let url = call.input.get("url").and_then(|v| v.as_str());
    if LEGACY_NET_TOOLS.contains(&name) {
        return Some(url);
    }
    if name.starts_with("mcp__") && URL_LEAVES.contains(&leaf(name)) {
        return url.map(Some);
    }
    None
}

/// Rule 1 (#2181, option A): a `write_file` / `edit_file` target that no grant
/// can authorise — the launch chain or an explicit `deny`. The ordinary write
/// scope stays with the tool's own `check_write_entitlement`; this only stops
/// the approval prompt from asking about a write the tool must refuse.
///
/// A relative path is skipped: it resolves against the session cwd, which the
/// hook context does not carry, and the tool still refuses it at execute.
pub(super) fn hard_refused_write(
    call: &ToolCall,
    agent_home: &Path,
    fs: &mur_common::agent::FilesystemEntitlement,
) -> Option<(HardWriteRefusal, PathBuf)> {
    use crate::tools::{edit_file::EDIT_FILE, write_file::WRITE_FILE};
    if !matches!(call.name(), WRITE_FILE | EDIT_FILE) {
        return None;
    }
    let raw = call.input.get("path")?.as_str()?;
    let target = canonical_target(raw)?;
    // Canonical home so the self-protected `deny` roots `for_file_tools`
    // adds compare against the same spelling as the canonical target
    // (`/var` vs `/private/var` on macOS).
    let home = std::fs::canonicalize(agent_home).unwrap_or_else(|_| agent_home.to_path_buf());
    let fs = for_file_tools(fs.clone(), &home);
    let chain = crate::sandbox::launch_chain::LaunchChain::new(&home);
    hard_write_refusal(&fs, &target, &chain).map(|r| (r, target))
}

/// The path the tool would check, for an absolute or `~` path: canonicalized
/// whole when it exists (`edit_file`), else parent + file name (`write_file`
/// creating a file). `None` for a relative path.
fn canonical_target(raw: &str) -> Option<PathBuf> {
    // The base is never used: only absolute results are kept below.
    let joined = crate::tools::fs_policy::resolve_path(Path::new(""), raw);
    if !joined.is_absolute() {
        return None;
    }
    if let Ok(c) = std::fs::canonicalize(&joined) {
        return Some(c);
    }
    let parent = joined.parent()?;
    let name = joined.file_name()?;
    Some(
        std::fs::canonicalize(parent)
            .unwrap_or_else(|_| parent.to_path_buf())
            .join(name),
    )
}

#[cfg(test)]
#[path = "tool_map_tests.rs"]
mod tool_map_tests;
