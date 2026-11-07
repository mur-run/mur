//! Resolve a parsed [`Mention`] against the agent and fleet namespaces
//! (at-mention design §3.5). Every namespace is consulted before deciding;
//! there is no precedence, so a collision is `AmbiguousTarget`, never a
//! silent pick.

use super::mention::{Mention, Namespace, is_reserved};
use crate::a2a_dial::{NameMatch, match_agent_name};

/// The single target a mention names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// Canonical on-disk agent directory name.
    Agent(String),
    /// Fleet name as stored.
    Fleet(String),
}

/// Why a mention names no single target. Each carries the typed name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// `UNKNOWN_TARGET`: nothing matches.
    UnknownTarget(String),
    /// `AMBIGUOUS_TARGET`: more than one match; use `@agent:` / `@fleet:`.
    AmbiguousTarget(String),
    /// The name is a reserved word.
    ReservedName(String),
}

/// `agents` are the directory names under `<mur_home>/agents/` that pass
/// `validate_agent_name`; `fleets` come from `fleet::store::list_fleets`.
/// Agent names match case-insensitively (exact spelling wins), the same rule
/// as [`crate::a2a_dial::canonicalize_agent_name`]; fleet names match exactly.
pub fn resolve(
    m: &Mention,
    agents: &[String],
    fleets: &[String],
) -> Result<Resolved, ResolveError> {
    let typed = &m.name;
    if is_reserved(typed) {
        return Err(ResolveError::ReservedName(typed.clone()));
    }
    let agent = || match_agent_name(agents, typed);
    let fleet = || fleets.iter().find(|f| *f == typed).cloned();
    match m.namespace {
        Some(Namespace::Agent) => match agent() {
            NameMatch::Found(name) => Ok(Resolved::Agent(name)),
            NameMatch::Missing => Err(ResolveError::UnknownTarget(typed.clone())),
            NameMatch::Ambiguous(_) => Err(ResolveError::AmbiguousTarget(typed.clone())),
        },
        Some(Namespace::Fleet) => fleet()
            .map(Resolved::Fleet)
            .ok_or_else(|| ResolveError::UnknownTarget(typed.clone())),
        // No precedence: every namespace is consulted, and more than one hit
        // (including several agent case variants) is ambiguous.
        None => match (agent(), fleet()) {
            (NameMatch::Found(name), None) => Ok(Resolved::Agent(name)),
            (NameMatch::Missing, Some(name)) => Ok(Resolved::Fleet(name)),
            (NameMatch::Missing, None) => Err(ResolveError::UnknownTarget(typed.clone())),
            _ => Err(ResolveError::AmbiguousTarget(typed.clone())),
        },
    }
}

#[cfg(test)]
#[path = "mention_resolve_tests.rs"]
mod tests;
