//! #1814: a riskless `bash: allow` must not carry a command whose
//! content-derived tier is above the session-grant ceiling past the gate.

use super::effective_call_policy;
use mur_common::agent::{ToolPolicy, ToolRule};
use serde_json::json;

fn allow(pattern: &str) -> ToolRule {
    ToolRule {
        pattern: pattern.into(),
        policy: ToolPolicy::Allow,
        risk: None,
        bypass_builtin_tier_floor: false,
    }
}

fn bash_policy(rules: &[ToolRule], cmd: &str) -> ToolPolicy {
    effective_call_policy(rules, "bash", Some(&json!({ "command": cmd })))
}

#[test]
fn bash_allow_asks_for_commands_above_the_ceiling() {
    let rules = [allow("bash")];
    for cmd in [
        "git push origin main",
        "rm -rf build",
        "sudo true",
        "cd repo && git push",
    ] {
        assert_eq!(bash_policy(&rules, cmd), ToolPolicy::Ask, "{cmd}");
    }
}

#[test]
fn bash_allow_still_runs_reads_and_local_writes() {
    let rules = [allow("bash")];
    for cmd in [
        "git status -sb",
        "ls -la",
        "cd repo && git log --oneline | head -5",
        "cargo build",
    ] {
        assert_eq!(bash_policy(&rules, cmd), ToolPolicy::Allow, "{cmd}");
    }
}

#[test]
fn bypass_on_the_deciding_rule_waives_the_bash_floor() {
    let mut waived = allow("bash");
    waived.bypass_builtin_tier_floor = true;
    assert_eq!(bash_policy(&[waived], "git push"), ToolPolicy::Allow);
}

#[test]
fn deny_and_missing_input_are_unchanged() {
    let mut deny = allow("bash");
    deny.policy = ToolPolicy::Deny;
    assert_eq!(bash_policy(&[deny], "ls"), ToolPolicy::Deny);
    // No input (preflight inventory) cannot be classified; the name-only
    // answer stands.
    assert_eq!(
        effective_call_policy(&[allow("bash")], "bash", None),
        ToolPolicy::Allow
    );
}

/// `bash_wait`/`bash_kill` resolve as `bash` for policy (D11) but carry a
/// job id, not a command — the content floor must not misread them.
#[test]
fn bash_control_tools_are_not_content_classified() {
    let rules = [allow("bash")];
    let input = json!({ "job_id": "rm -rf /" });
    for tool in ["bash_wait", "bash_kill"] {
        assert_eq!(
            effective_call_policy(&rules, tool, Some(&input)),
            ToolPolicy::Allow,
            "{tool}"
        );
    }
}
