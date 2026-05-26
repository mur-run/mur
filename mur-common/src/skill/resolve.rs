use crate::skill::manifest::ProcedureStep;
use crate::skill::mcp::{McpRequirement, SkillCapability};
use crate::skill::inventory::McpInventory;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Step had a literal `tool` and no `intent` — pre-M6b path.
    Literal { tool: String },
    /// `tool_hint` matched an inventory entry.
    Hint { tool: String },
    /// An `mcp_requirements` glob matched at least one inventory tool.
    IntentMatch {
        tool: String,
        capability: SkillCapability,
    },
    /// No glob match, but the requirement's `fallback` exists in the inventory.
    Fallback {
        tool: String,
        capability: SkillCapability,
    },
    /// No usable tool.
    Unresolved { reason: String },
}

impl Resolution {
    pub fn picked_tool(&self) -> Option<&str> {
        match self {
            Resolution::Literal { tool }
            | Resolution::Hint { tool }
            | Resolution::IntentMatch { tool, .. }
            | Resolution::Fallback { tool, .. } => Some(tool),
            Resolution::Unresolved { .. } => None,
        }
    }

    pub fn source_tag(&self) -> &'static str {
        match self {
            Resolution::Literal { .. } => "literal",
            Resolution::Hint { .. } => "hint",
            Resolution::IntentMatch { .. } => "intent_match",
            Resolution::Fallback { .. } => "fallback",
            Resolution::Unresolved { .. } => "unresolved",
        }
    }
}

/// Resolve a single procedure step against the agent's MCP inventory.
///
/// Decision tree:
/// 1. `tool` set + `intent` unset → `Literal(tool)` (pre-M6b behaviour).
/// 2. `tool_hint` set + inventory match → `Hint(tool)`.
/// 3. `intent` set + `mcp_requirements` glob matches an inventory tool →
///    `IntentMatch(shortest tool, capability)`.
/// 4. `intent` set + no glob match but `fallback` in inventory → `Fallback(fallback)`.
/// 5. Otherwise → `Unresolved { reason }`.
pub fn resolve_step(
    step: &ProcedureStep,
    requirements: &[McpRequirement],
    inventory: &McpInventory,
) -> Resolution {
    // Rule 1 — literal tool, no intent (pre-M6b path).
    if step.intent.is_none() {
        if let Some(t) = step.tool.as_deref() {
            return Resolution::Literal { tool: t.to_string() };
        }
        return Resolution::Unresolved {
            reason: "step has neither tool nor intent".into(),
        };
    }

    // Rule 2 — tool_hint match.
    if let Some(hint) = step.tool_hint.as_deref() {
        if let Some(t) = match_in_inventory(hint, inventory) {
            return Resolution::Hint { tool: t };
        }
    }

    // Rule 3 — intent_match via mcp_requirements globs.
    let mut best: Option<(String, SkillCapability)> = None;
    for req in requirements {
        let Ok(glob) = globset::Glob::new(&req.tool_pattern) else {
            continue;
        };
        let matcher = glob.compile_matcher();
        let mut candidates: Vec<&str> = inventory.iter().filter(|t| matcher.is_match(t)).collect();
        if candidates.is_empty() {
            continue;
        }
        // Shortest name wins; lexicographically smallest on ties.
        candidates.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        let pick = candidates[0].to_string();
        best = Some((pick, req.capability));
        break;
    }
    if let Some((tool, cap)) = best {
        return Resolution::IntentMatch {
            tool,
            capability: cap,
        };
    }

    // Rule 4 — fallback.
    for req in requirements {
        if req.fallback.is_empty() {
            continue;
        }
        if inventory.contains(&req.fallback) {
            return Resolution::Fallback {
                tool: req.fallback.clone(),
                capability: req.capability,
            };
        }
    }

    // Rule 5 — unresolved.
    Resolution::Unresolved {
        reason: format!(
            "intent '{}' has no matching tool in inventory ({} tools, {} requirements)",
            step.intent.as_deref().unwrap_or(""),
            inventory.iter().count(),
            requirements.len(),
        ),
    }
}

fn match_in_inventory(pattern: &str, inv: &McpInventory) -> Option<String> {
    if pattern.contains('*') {
        let Ok(g) = globset::Glob::new(pattern) else {
            return None;
        };
        let m = g.compile_matcher();
        let mut hits: Vec<&str> = inv.iter().filter(|t| m.is_match(t)).collect();
        hits.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        hits.first().map(|s| s.to_string())
    } else {
        inv.contains(pattern).then(|| pattern.to_string())
    }
}
