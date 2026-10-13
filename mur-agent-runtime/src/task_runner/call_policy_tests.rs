//! #1814: a riskless `bash: allow` must not carry a command whose
//! content-derived tier is above the session-grant ceiling past the gate.

use super::{approval_risk, effective_call_policy};
use mur_common::agent::{ToolPolicy, ToolRule};
use mur_common::hitl::RiskTier;
use mur_common::hitl::floor_waiver::FloorWaiver;
use serde_json::json;

fn allow(pattern: &str) -> ToolRule {
    ToolRule {
        pattern: pattern.into(),
        policy: ToolPolicy::Allow,
        risk: None,
        bypass_builtin_tier_floor: FloorWaiver::Off,
    }
}

fn waived(pattern: &str, waiver: FloorWaiver) -> ToolRule {
    ToolRule {
        bypass_builtin_tier_floor: waiver,
        ..allow(pattern)
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

/// A named tier lifts the ceiling up to that tier and no further.
#[test]
fn a_named_tier_waives_the_bash_floor_up_to_that_tier() {
    let egress = [waived("bash", FloorWaiver::UpTo(RiskTier::NetworkEgress))];
    assert_eq!(bash_policy(&egress, "git push"), ToolPolicy::Allow);
    assert_eq!(bash_policy(&egress, "rm -rf build"), ToolPolicy::Ask);
    assert_eq!(bash_policy(&egress, "ls"), ToolPolicy::Allow);
}

/// BREAKING vs #1833: `true` on `bash` used to waive every command, `sudo`
/// included — fail-open in one line. It is now ignored: the rule behaves
/// exactly as if the waiver were absent.
#[test]
fn true_on_bash_waives_nothing() {
    let all = [waived("bash", FloorWaiver::All)];
    for cmd in ["git push", "rm -rf build", "sudo true", "chmod 777 x"] {
        assert_eq!(bash_policy(&all, cmd), ToolPolicy::Ask, "{cmd}");
    }
    assert_eq!(bash_policy(&all, "ls -la"), ToolPolicy::Allow);
}

/// The highest waiver still leaves privileged commands asking.
#[test]
fn privileged_always_asks_on_bash() {
    for w in [
        FloorWaiver::UpTo(RiskTier::Destructive),
        FloorWaiver::UpTo(RiskTier::Privileged),
    ] {
        let rules = [waived("bash", w)];
        assert_eq!(bash_policy(&rules, "rm -rf build"), ToolPolicy::Allow);
        for cmd in ["sudo true", "chmod 777 x", "cd x && sudo ls"] {
            assert_eq!(bash_policy(&rules, cmd), ToolPolicy::Ask, "{w:?} {cmd}");
        }
    }
}

/// #1829: a glob never carries the waiver, even when it decides `bash`.
#[test]
fn a_glob_waiver_does_not_lift_the_bash_ceiling() {
    let glob = [waived("*", FloorWaiver::UpTo(RiskTier::Destructive))];
    assert_eq!(bash_policy(&glob, "git push"), ToolPolicy::Ask);
}

/// The prompt names the tier that made the call ask, so no client can
/// session-grant it as a plain write; a read carries nothing.
#[test]
fn the_prompt_carries_the_command_tier() {
    let rules = [allow("bash")];
    let risk = |cmd: &str| approval_risk(&rules, "bash", Some(&json!({ "command": cmd })));
    assert_eq!(risk("sudo true"), Some(RiskTier::Privileged));
    assert_eq!(risk("rm -rf build"), Some(RiskTier::Destructive));
    assert_eq!(risk("ls"), None);
}

/// A browser tool's floor is fixed, so `true` there keeps meaning "all".
#[test]
fn true_still_waives_a_browser_tool() {
    let click = "mcp__browser__browser_click";
    let rules = [waived(click, FloorWaiver::All)];
    assert_eq!(
        effective_call_policy(&rules, click, Some(&json!({}))),
        ToolPolicy::Allow
    );
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
