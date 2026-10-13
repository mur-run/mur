//! `bypass_builtin_tier_floor` — how far one exact-name rule lifts the
//! built-in tier floor (#1786, #1814).
//!
//! Two shapes, both written on the rule:
//!
//! - `true` — the #1786 form. Waives a browser tool's floor whole: a browser
//!   tool's tier is fixed, so "all of it" is a known amount. It means nothing
//!   on `bash`, whose tier depends on the command — there `true` would be "any
//!   command at all", the one-line release #1811/#1829 closed for browser
//!   servers. So on `bash` it is IGNORED (warned at start, flagged by
//!   `doctor` / `perm list-tools`).
//! - a tier name (`network-egress`, `destructive`, ...) — "waive up to this
//!   tier". Self-describing in review, and stable when the classifier changes.
//!
//! `privileged` is never waived on `bash`: a `sudo` or `chmod` always asks.
//!
//! The runtime gate (`call_floor`) reads [`bash_ceiling`]: a `bash: allow`
//! call above it asks, and each such call leaves a
//! `mur.hitl.bash_ceiling_exceeded` record (`enforced: true`) for doctor.
//!
//! BREAKING vs #1833, which shipped `bool`: there `true` on `bash` waived
//! every command, `sudo` included. It is now ignored — the rule behaves as if
//! the waiver were absent — and warned about at start, in `doctor`, and in
//! `perm list-tools`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::RiskTier;

/// The tool name the bash ceiling applies to. Exact-name only (#1829): a glob
/// cannot carry a waiver.
pub const BASH_TOOL: &str = "bash";

/// The highest tier a `bash` waiver can reach. `Privileged` always asks.
pub const BASH_WAIVER_MAX: RiskTier = RiskTier::Destructive;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FloorWaiver {
    /// No waiver (`false` or absent).
    #[default]
    Off,
    /// `true`: waive a browser tool's floor whole. Ignored on `bash`.
    All,
    /// A tier name: waive up to and including this tier.
    UpTo(RiskTier),
}

impl FloorWaiver {
    pub fn is_off(&self) -> bool {
        *self == FloorWaiver::Off
    }

    /// Whether this waiver releases a browser tool whose built-in tier is
    /// `tier`.
    pub fn covers(&self, tier: RiskTier) -> bool {
        match self {
            FloorWaiver::Off => false,
            FloorWaiver::All => true,
            FloorWaiver::UpTo(max) => tier <= *max,
        }
    }
}

impl Serialize for FloorWaiver {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            FloorWaiver::Off => s.serialize_bool(false),
            FloorWaiver::All => s.serialize_bool(true),
            FloorWaiver::UpTo(t) => t.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for FloorWaiver {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Tier(RiskTier),
        }
        Ok(
            match Raw::deserialize(d).map_err(|_| {
                serde::de::Error::custom(
                    "bypass_builtin_tier_floor: expected true/false or a tier \
                 (write, network-egress, spend, destructive)",
                )
            })? {
                Raw::Bool(false) => FloorWaiver::Off,
                Raw::Bool(true) => FloorWaiver::All,
                Raw::Tier(t) => FloorWaiver::UpTo(t),
            },
        )
    }
}

/// The tier a riskless `allow` on `bash` lets through unattended. `rule` is
/// the DECIDING rule for `bash`; only an exact-name rule's waiver counts.
pub fn bash_ceiling(rule: Option<&crate::agent::ToolRule>) -> RiskTier {
    let base = RiskTier::Write;
    match rule {
        Some(r) if r.pattern == BASH_TOOL => match r.bypass_builtin_tier_floor {
            FloorWaiver::UpTo(t) => t.min(BASH_WAIVER_MAX).max(base),
            FloorWaiver::Off | FloorWaiver::All => base,
        },
        _ => base,
    }
}

/// Start-of-agent warnings for waivers that do not mean what they say. One
/// line each, never an error — nobody's agent stops starting over this.
pub fn waiver_warnings(rules: &[crate::agent::ToolRule]) -> Vec<String> {
    let mut out = Vec::new();
    for r in rules.iter().filter(|r| r.pattern == BASH_TOOL) {
        match r.bypass_builtin_tier_floor {
            FloorWaiver::All => out.push(format!(
                "profile.yaml tool rule `{BASH_TOOL}`: bypass_builtin_tier_floor: true is IGNORED \
                 (it would waive every command); name a ceiling instead, e.g. network-egress (#1814)"
            )),
            FloorWaiver::UpTo(RiskTier::Privileged) => out.push(format!(
                "profile.yaml tool rule `{BASH_TOOL}`: bypass_builtin_tier_floor: privileged is capped \
                 at destructive — privileged commands always ask (#1814)"
            )),
            _ => {}
        }
    }
    out
}

/// A `bash` call above the agent's bash ceiling: under an `allow` rule the
/// gate asks for it, and the runtime records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeilingHit {
    pub tier: RiskTier,
    pub ceiling: RiskTier,
    /// Pattern of the deciding rule (`None` = no rule).
    pub rule: Option<String>,
}

/// Is `tool_name` with `input` above the bash ceiling? `None` for any tool
/// but `bash`, and for a command at or under the ceiling. Says nothing about
/// the rule's policy — the caller checks that it was an `allow`.
pub fn bash_ceiling_hit(
    rules: &[crate::agent::ToolRule],
    tool_name: &str,
    input: Option<&serde_json::Value>,
) -> Option<CeilingHit> {
    if tool_name != BASH_TOOL {
        return None;
    }
    let rule = crate::agent::resolve_tool_rule(rules, BASH_TOOL);
    let ceiling = bash_ceiling(rule);
    let tier = super::tool_tier::classify(BASH_TOOL, input);
    (tier > ceiling).then(|| CeilingHit {
        tier,
        ceiling,
        rule: rule.map(|r| r.pattern.clone()),
    })
}

#[cfg(test)]
mod tests;
