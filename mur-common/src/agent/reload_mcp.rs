//! The `mcp_servers` half of [`AgentProfile::reload_diff`].
//!
//! The rest of the profile is compared path by path; `mcp_servers` is a list,
//! so a positional compare would call every entry after an insertion changed.
//! Here entries are matched by `name`, and each matched pair is classified as
//! a whole (D9): one cold sub-field makes the entry cold, and nothing in it is
//! applied. A hot entry is reported as `mcp_servers.<name>`; a cold one as
//! `mcp_servers.<name>.<field>` for every cold field that changed.
//!
//! Like the outer walk, the classification names only the hot fields, so a key
//! added to [`McpServerEntry`] later (or an unknown key a newer `mur` wrote,
//! kept in `extra`) is cold by default.
//!
//! "Hot" here means *eligible*. A changed `command` additionally needs a
//! positive pin result and an exec-allowed path at reload time; that gate
//! lives in the runtime, which can hash the binary. What this module rules
//! out without touching the disk is everything the gate could never accept.

use super::{AgentProfile, McpServerEntry, ReloadDiff};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Top-level key the keyed diff owns.
pub(super) const MCP_SERVERS: &str = "mcp_servers";

/// Entry fields that may change under a running agent. `command` is listed
/// but gated (see [`classify_entry`]); `network` is split by sub-field.
const HOT_ENTRY_FIELDS: &[&str] = &[
    "args",
    "command",
    "timeout_secs",
    "binary_sha256",
    "description_hash",
    "publisher",
    "installed_at",
];

/// `network` sub-fields that may change: they are bound to the new client
/// generation's proxy token. `mode` and `authorization` widen or re-authorize
/// egress and stay cold.
const HOT_NETWORK_FIELDS: &[&str] = &["allow_hosts", "deny_hosts"];

/// Add the `mcp_servers` changes between `old` and `new` to `diff`.
pub(super) fn diff_mcp_servers(old: &AgentProfile, new: &AgentProfile, diff: &mut ReloadDiff) {
    let (Some(a), Some(b)) = (by_name(&old.mcp_servers), by_name(&new.mcp_servers)) else {
        // Duplicate names: there is no key to match on, so nothing is hot.
        if old.mcp_servers != new.mcp_servers {
            diff.cold.push(MCP_SERVERS.into());
        }
        return;
    };
    if !a.keys().eq(b.keys()) {
        // D3: adding or removing a server is a restart.
        diff.cold.push(MCP_SERVERS.into());
        return;
    }
    for (name, old_entry) in &a {
        let new_entry = b[name];
        match classify_entry(old_entry, new_entry) {
            EntryChange::Same => {}
            EntryChange::Hot => diff.hot.push(format!("{MCP_SERVERS}.{name}")),
            EntryChange::Cold(fields) => diff.cold.extend(
                fields
                    .into_iter()
                    .map(|f| format!("{MCP_SERVERS}.{name}.{f}")),
            ),
        }
    }
}

/// The entries a reload serves: an entry whose change from `base` (boot)
/// [`diff_mcp_servers`] would report hot is taken from `newer`; an unchanged
/// one is boot's; a cold one keeps what `served` (the last reload) holds, so a
/// cold edit never rolls back a hot edit already applied. Classifying against
/// boot keeps every cold field at its boot value. A changed set of names is a
/// restart (D3), so nothing in it is applied and `served` stays.
/// Order follows `base`.
pub(super) fn overlay_hot_entries(
    base: &[McpServerEntry],
    served: &[McpServerEntry],
    newer: &[McpServerEntry],
) -> Vec<McpServerEntry> {
    let kept = |e: &McpServerEntry| {
        served
            .iter()
            .find(|s| s.name == e.name)
            .unwrap_or(e)
            .clone()
    };
    let (Some(a), Some(b)) = (by_name(base), by_name(newer)) else {
        return base.iter().map(kept).collect();
    };
    if !a.keys().eq(b.keys()) {
        return base.iter().map(kept).collect();
    }
    base.iter()
        .map(|e| {
            let n = b[e.name.as_str()];
            match classify_entry(e, n) {
                EntryChange::Hot => n.clone(),
                EntryChange::Same => e.clone(),
                EntryChange::Cold(_) => kept(e),
            }
        })
        .collect()
}

fn by_name(entries: &[McpServerEntry]) -> Option<BTreeMap<&str, &McpServerEntry>> {
    let mut m = BTreeMap::new();
    for e in entries {
        if m.insert(e.name.as_str(), e).is_some() {
            return None;
        }
    }
    Some(m)
}

#[derive(Debug, PartialEq, Eq)]
enum EntryChange {
    Same,
    Hot,
    /// The cold fields that changed, sorted.
    Cold(Vec<String>),
}

fn classify_entry(old: &McpServerEntry, new: &McpServerEntry) -> EntryChange {
    let (Ok(Value::Object(a)), Ok(Value::Object(b))) =
        (serde_json::to_value(old), serde_json::to_value(new))
    else {
        return EntryChange::Cold(vec!["<entry>".into()]);
    };
    let changed = changed_keys(&a, &b);
    if changed.is_empty() {
        return EntryChange::Same;
    }
    let mut cold = Vec::new();
    for key in &changed {
        match key.as_str() {
            "network" => cold.extend(network_cold(a.get("network"), b.get("network"))),
            k if HOT_ENTRY_FIELDS.contains(&k) => {}
            _ => cold.push(key.clone()),
        }
    }
    let launch_changed = changed.iter().any(|k| k == "command" || k == "args");
    // D7: an interpreter's args name the code it runs, and its binary hash
    // says nothing about that code; any launch change is a restart.
    if launch_changed
        && (crate::exec::is_interpreter_command(&old.command)
            || crate::exec::is_interpreter_command(&new.command))
    {
        cold.extend(
            changed
                .iter()
                .filter(|k| *k == "command" || *k == "args")
                .cloned(),
        );
    }
    // D7: a new binary needs a pin to verify against. Without one the
    // runtime's gate could only ever answer `Skipped`, which is cold.
    if changed.iter().any(|k| k == "command")
        && new.binary_sha256.is_none()
        && new.package.is_none()
    {
        cold.push("command".into());
    }
    if cold.is_empty() {
        return EntryChange::Hot;
    }
    cold.sort();
    cold.dedup();
    EntryChange::Cold(cold)
}

fn changed_keys(a: &Map<String, Value>, b: &Map<String, Value>) -> Vec<String> {
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}

/// Cold paths inside a changed `network`. Gaining or losing the block is a
/// mode change (absent means `inherit`), so it is cold as a whole.
fn network_cold(a: Option<&Value>, b: Option<&Value>) -> Vec<String> {
    let (Some(Value::Object(a)), Some(Value::Object(b))) = (a, b) else {
        return vec!["network".into()];
    };
    changed_keys(a, b)
        .into_iter()
        .filter(|k| !HOT_NETWORK_FIELDS.contains(&k.as_str()))
        .map(|k| format!("network.{k}"))
        .collect()
}

#[cfg(test)]
#[path = "reload_mcp_tests.rs"]
mod tests;
