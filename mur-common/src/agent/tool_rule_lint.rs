//! Case-only misspellings in `ToolRule.pattern` (#2147).
//!
//! Rules match tool names exactly and case-sensitively — MCP tool names are
//! case-sensitive by spec, so two real tools may differ only by case, and
//! folding at match time would be wrong. The cost is that `Write_file` matches
//! nothing: an `allow` silently falls through to `ask` (fails closed), and a
//! `deny`/`ask`/`risk:` rule silently does nothing (fails OPEN). This module
//! finds those rules so the CLI, `doctor`, and the runtime can say so.

use super::{ToolPolicy, ToolRule};

/// Every tool name the runtime registers itself (MCP tools excluded — those
/// are `mcp__<server>__<tool>`). `mur-agent-runtime` has a test that keeps
/// this in step with its `ToolExecutor` names.
pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "bash",
    "bash_wait",
    "bash_kill",
    "read_file",
    "write_file",
    "edit_file",
    "suggest_replies",
    crate::proposal::PROPOSE_TOOL,
    "recall",
    "remember",
    "remind",
    "open_item",
    "fleet_run",
    "git_push_request",
    "git_push_status",
    "git_push_cancel",
];

/// What a rule's pattern can be checked against.
#[derive(Debug, Default, Clone)]
pub struct KnownTools {
    /// Full tool names (builtins, plus discovered MCP wire names if known).
    pub names: Vec<String>,
    /// `mcp__<server>__` prefixes for configured servers whose tool lists are
    /// not known here; anything after an exact prefix is taken on trust.
    pub prefixes: Vec<String>,
}

impl KnownTools {
    /// Builtins plus one prefix per configured MCP server.
    pub fn for_servers<'a>(servers: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            names: BUILTIN_TOOL_NAMES.iter().map(|s| s.to_string()).collect(),
            prefixes: servers
                .into_iter()
                .map(|s| crate::mcp_naming::wire_name(&crate::mcp_naming::sanitize_server(s), ""))
                .collect(),
        }
    }

    /// Exactly these names (the runtime's registered set).
    pub fn names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            names: names.into_iter().map(str::to_string).collect(),
            prefixes: Vec::new(),
        }
    }
}

/// The correct spelling of `pattern` when it matches no known tool exactly
/// but does match one ignoring ASCII case. `None` = the pattern is live, or
/// unrecognisable either way (not a case problem). Trailing-`*` globs are
/// checked by their prefix and the suggestion keeps the `*`.
pub fn case_fix(pattern: &str, known: &KnownTools) -> Option<String> {
    let (body, glob) = match pattern.strip_suffix('*') {
        Some(prefix) => (prefix, true),
        None => (pattern, false),
    };
    let lower = body.to_ascii_lowercase();
    let live = |cand: &str| {
        if glob {
            cand.starts_with(body)
        } else {
            cand == body
        }
    };
    if known.names.iter().any(|n| live(n))
        || known
            .prefixes
            .iter()
            .any(|p| body.starts_with(p.as_str()) || (glob && p.starts_with(body)))
    {
        return None;
    }
    let star = if glob { "*" } else { "" };
    for n in &known.names {
        let hit = if glob {
            n.to_ascii_lowercase().starts_with(&lower)
        } else {
            n.eq_ignore_ascii_case(body)
        };
        if hit && let Some(fixed) = n.get(..body.len()) {
            return Some(format!("{fixed}{star}"));
        }
    }
    for p in &known.prefixes {
        let p_lower = p.to_ascii_lowercase();
        if lower.starts_with(&p_lower)
            && let Some(rest) = body.get(p.len()..)
        {
            return Some(format!("{p}{rest}{star}"));
        }
        // A glob shorter than the prefix (`MCP__*` for `mcp__github__`):
        // keep the length the operator wrote.
        if glob
            && p_lower.starts_with(&lower)
            && let Some(fixed) = p.get(..body.len())
        {
            return Some(format!("{fixed}{star}"));
        }
    }
    None
}

/// One rule whose pattern is a case-only misspelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadRule {
    pub pattern: String,
    pub suggestion: String,
    pub policy: ToolPolicy,
    /// The rule restricts (`deny`/`ask`, or declares a `risk:`), so being dead
    /// means the restriction is NOT in effect — it fails open.
    pub fails_open: bool,
}

impl DeadRule {
    /// Operator-facing explanation, shared by `perm` and `doctor`.
    pub fn message(&self) -> String {
        let policy = format!("{:?}", self.policy).to_lowercase();
        let effect = if self.fails_open {
            format!(
                "NOT IN EFFECT — `{}` is not restricted by it",
                self.suggestion
            )
        } else {
            format!(
                "dead — `{}` falls through to the default (ask)",
                self.suggestion
            )
        };
        format!(
            "tool rule `{policy} {}` matches no tool; did you mean `{}`? (names are case-sensitive) {effect}",
            self.pattern, self.suggestion
        )
    }
}

/// Every rule in `rules` whose pattern is a case-only misspelling.
pub fn dead_rules(rules: &[ToolRule], known: &KnownTools) -> Vec<DeadRule> {
    rules
        .iter()
        .filter_map(|r| {
            case_fix(&r.pattern, known).map(|suggestion| DeadRule {
                pattern: r.pattern.clone(),
                suggestion,
                policy: r.policy,
                fails_open: r.policy != ToolPolicy::Allow || r.risk.is_some(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, policy: ToolPolicy) -> ToolRule {
        ToolRule {
            pattern: pattern.into(),
            policy,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }
    }

    fn known() -> KnownTools {
        KnownTools::for_servers(["github"])
    }

    #[test]
    fn miscased_builtin_names_the_real_spelling() {
        assert_eq!(
            case_fix("Write_file", &known()).as_deref(),
            Some("write_file")
        );
        assert_eq!(case_fix("BASH", &known()).as_deref(), Some("bash"));
    }

    #[test]
    fn exact_and_unknown_patterns_are_not_flagged() {
        assert_eq!(case_fix("write_file", &known()), None);
        assert_eq!(case_fix("no_such_tool", &known()), None);
        assert_eq!(
            case_fix("mcp__github__Merge_PR", &known()),
            None,
            "tool part is the server's"
        );
        assert_eq!(case_fix("mcp__github__*", &known()), None);
        assert_eq!(case_fix("mcp__*", &known()), None);
        assert_eq!(case_fix("*", &known()), None);
    }

    #[test]
    fn miscased_globs_and_server_prefixes_are_fixed() {
        assert_eq!(case_fix("Bash*", &known()).as_deref(), Some("bash*"));
        assert_eq!(
            case_fix("mcp__GitHub__*", &known()).as_deref(),
            Some("mcp__github__*")
        );
        assert_eq!(
            case_fix("MCP__github__merge_pr", &known()).as_deref(),
            Some("mcp__github__merge_pr")
        );
    }

    #[test]
    fn miscased_globs_shorter_than_a_server_prefix_are_fixed() {
        // The glob covers the whole `mcp__github__` prefix; the suggestion
        // keeps the length the operator wrote.
        assert_eq!(case_fix("MCP__*", &known()).as_deref(), Some("mcp__*"));
        assert_eq!(
            case_fix("mcp__Git*", &known()).as_deref(),
            Some("mcp__git*")
        );
        assert_eq!(case_fix("mcp__Gitlab*", &known()), None, "no such server");
    }

    #[test]
    fn miscased_deny_is_reported_as_failing_open() {
        let rules = [
            rule("Bash", ToolPolicy::Deny),
            rule("Write_file", ToolPolicy::Allow),
            rule("read_file", ToolPolicy::Deny),
        ];
        let dead = dead_rules(&rules, &known());
        assert_eq!(dead.len(), 2);
        assert!(dead[0].fails_open);
        assert_eq!(dead[0].suggestion, "bash");
        assert!(
            dead[0].message().contains("NOT IN EFFECT"),
            "{}",
            dead[0].message()
        );
        assert!(!dead[1].fails_open);
        assert!(dead[1].message().contains("did you mean `write_file`"));
    }

    #[test]
    fn a_risk_on_an_allow_also_fails_open() {
        let mut r = rule("Write_file", ToolPolicy::Allow);
        r.risk = Some(crate::hitl::RiskTier::Destructive);
        assert!(dead_rules(&[r], &known())[0].fails_open);
    }

    #[test]
    fn a_real_tool_that_differs_only_by_case_is_live() {
        // Two real tools may differ only by case; the exact one is live.
        let k = KnownTools::names(["mcp__s__Run", "mcp__s__run"]);
        assert_eq!(case_fix("mcp__s__Run", &k), None);
        assert_eq!(case_fix("mcp__s__RUN", &k).as_deref(), Some("mcp__s__Run"));
    }
}
