//! Entry names for the wizard's serena targets (#2016 Decision 4). An
//! entry's identity is its canonical project path: a project an agent
//! already serves reuses that entry under whatever name it has, and a name
//! already serving another path is never repointed.

use std::path::{Path, PathBuf};

use mur_agent_runtime::mcp::serena::{MAX_ENTRY_NAME_LEN, serena_paths_for, tool_prefix_clash};
use mur_common::AgentProfile;
use mur_common::agent::McpServerKind;

use super::super::consent::Manifest;

/// Used when a project's directory name sanitizes to nothing.
pub(super) const FALLBACK_NAME: &str = "project";

/// `raw` as an entry name serena will accept (`serena_paths_for`): lower
/// case `[a-z0-9_-]`, at most [`MAX_ENTRY_NAME_LEN`], no doubled or
/// trailing `_`. `mur.run-website` → `mur-run-website`.
pub(super) fn sanitize(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'a'..='z' | '0'..='9' | '_' | '-' => c,
            _ => '-',
        };
        let sep = |x: char| x == '-' || x == '_';
        if sep(c) && out.chars().last().is_none_or(sep) {
            continue;
        }
        out.push(c);
    }
    out.truncate(MAX_ENTRY_NAME_LEN);
    let out = out.trim_end_matches(['-', '_']).to_owned();
    if valid(&out) {
        out
    } else {
        FALLBACK_NAME.to_owned()
    }
}

/// The check spawn applies: the home is derived from the name alone.
pub(super) fn valid(name: &str) -> bool {
    serena_paths_for(Path::new(""), name).is_ok()
}

/// One agent's existing serena entries, read from its profile and manifest.
#[derive(Debug, Default)]
pub(super) struct Existing {
    /// Every MCP server name in the profile.
    pub(super) servers: Vec<String>,
    /// `kind: serena` entries with a manifest record, in the order the
    /// manifest is read: the default `serena` entry, then by name.
    pub(super) known: Vec<(String, PathBuf)>,
    /// `kind: serena` entries with no manifest record (hand-edited).
    pub(super) unknown: Vec<String>,
}

impl Existing {
    pub(super) fn read(profile: &AgentProfile, m: &Manifest) -> Self {
        let servers: Vec<String> = profile.mcp_servers.iter().map(|s| s.name.clone()).collect();
        let mut known = Vec::new();
        let mut unknown = Vec::new();
        let mut serena: Vec<&str> = profile
            .mcp_servers
            .iter()
            .filter(|s| s.kind == Some(McpServerKind::Serena))
            .map(|s| s.name.as_str())
            .collect();
        let default = mur_agent_runtime::mcp::serena::DEFAULT_ENTRY_NAME;
        serena.sort_by_key(|n| (*n != default, *n));
        for name in serena {
            match m.project_of(name) {
                Some(p) => {
                    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
                    known.push((name.to_owned(), p));
                }
                None => unknown.push(name.to_owned()),
            }
        }
        Self {
            servers,
            known,
            unknown,
        }
    }

    /// Whether this agent already serves `project` (canonical).
    pub(super) fn serves(&self, project: &Path) -> bool {
        self.known.iter().any(|(_, p)| p == project)
    }

    fn taken(&self, name: &str, also: &[String]) -> bool {
        self.servers.iter().any(|s| s == name)
            || self.known.iter().any(|(n, _)| n == name)
            || also.iter().any(|s| s == name)
            || tool_prefix_clash(name, self.servers.iter().chain(also).map(String::as_str))
                .is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Naming {
    /// An entry already serves this path: keep its name. `duplicates` are
    /// other entries serving the same path (warned about, never removed).
    Reuse {
        entry: String,
        duplicates: Vec<String>,
    },
    /// A new entry. `wanted` is set when `entry` had to be suffixed.
    New {
        entry: String,
        wanted: Option<String>,
    },
}

impl Naming {
    pub(super) fn entry(&self) -> &str {
        match self {
            Naming::Reuse { entry, .. } | Naming::New { entry, .. } => entry,
        }
    }
}

/// The entry `project` gets on this agent. `base` is the sanitized project
/// name; `also` holds names this run already assigned on the same agent.
pub(super) fn name_for(ex: &Existing, project: &Path, base: &str, also: &[String]) -> Naming {
    let mut serving = ex.known.iter().filter(|(_, p)| p == project);
    if let Some((first, _)) = serving.next() {
        return Naming::Reuse {
            entry: first.clone(),
            duplicates: serving.map(|(n, _)| n.clone()).collect(),
        };
    }
    if !ex.taken(base, also) && valid(base) {
        return Naming::New {
            entry: base.to_owned(),
            wanted: None,
        };
    }
    Naming::New {
        entry: suffixed(base, |c| ex.taken(c, also) || !valid(c)),
        wanted: Some(base.to_owned()),
    }
}

/// Whether an edited name is usable for a new entry on this agent.
pub(super) fn usable(ex: &Existing, name: &str, also: &[String]) -> bool {
    valid(name) && !ex.taken(name, also)
}

/// `<base>-2`, `-3`, … the first one `taken` refuses not.
fn suffixed(base: &str, taken: impl Fn(&str) -> bool) -> String {
    (2u32..)
        .map(|n| {
            let tail = format!("-{n}");
            let keep = MAX_ENTRY_NAME_LEN.saturating_sub(tail.len());
            let head = base[..base.len().min(keep)].trim_end_matches(['-', '_']);
            format!("{head}{tail}")
        })
        .find(|c| !taken(c))
        .expect("an unbounded suffix range always has a free name")
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod tests;
