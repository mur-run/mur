//! The CLI's "don't ask again this session" decision, for other clients.
//!
//! The Hub remembers session answers on its side, exactly as the CLI does in
//! `session_tool_allow`. What it must NOT do is decide on its own what a
//! session answer may cover: that decision is `dest::grant_for` over the
//! gate's `effective_tier`, and a second copy of it would drift. So this is
//! the one public door to it — the same tier, the same key, the same label.

use mur_common::hitl::RiskTier;
use serde::Serialize;

/// A grantable session answer: the key to remember and the row's wording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionGrant {
    pub key: String,
    pub label: String,
}

/// What "don't ask again this session" would cover for this call, or `None`
/// when the call must keep asking (force pushes, protected branches, tiers
/// above the ceiling with no proved scope).
pub fn session_grant(
    tool_name: &str,
    tool_input: &serde_json::Value,
    declared_risk: Option<RiskTier>,
) -> Option<SessionGrant> {
    let tier = super::tool_tier::effective_tier(tool_name, Some(tool_input), declared_risk);
    let grant = super::dest::grant_for(tool_name, Some(tool_input), tier);
    let key = grant.key()?;
    Some(SessionGrant {
        key,
        label: grant.label(),
    })
}

#[cfg(test)]
mod tests;
