use super::ask_origin;
use crate::task_runner::effective_call_policy;
use mur_common::agent::{ToolPolicy, ToolRule};
use mur_common::hitl::RiskTier;
use mur_common::hitl::floor_waiver::FloorWaiver;
use mur_common::hitl::startup_waiver::AskOrigin;
use serde_json::{Value, json};

fn rule(pattern: &str, policy: ToolPolicy, risk: Option<RiskTier>) -> ToolRule {
    ToolRule {
        pattern: pattern.into(),
        policy,
        risk,
        bypass_builtin_tier_floor: FloorWaiver::Off,
    }
}

struct Case {
    name: &'static str,
    rules: Vec<ToolRule>,
    tool: &'static str,
    input: Value,
    lift: Option<RiskTier>,
    want: AskOrigin,
}

fn flags(explicit: bool, default: bool, clamp: bool, intent_raised: bool) -> AskOrigin {
    AskOrigin {
        explicit,
        default,
        clamp,
        intent_raised,
        hook_raised: false,
    }
}

fn table() -> Vec<Case> {
    use ToolPolicy::{Allow, Ask};
    let none = json!({});
    vec![
        Case {
            name: "explicit ask",
            rules: vec![rule("write_file", Ask, None)],
            tool: "write_file",
            input: none.clone(),
            lift: None,
            want: flags(true, false, false, false),
        },
        Case {
            name: "no rule",
            rules: vec![],
            tool: "write_file",
            input: none.clone(),
            lift: None,
            want: flags(false, true, false, false),
        },
        Case {
            name: "allow + risk: destructive",
            rules: vec![rule("mcp__x__drop", Allow, Some(RiskTier::Destructive))],
            tool: "mcp__x__drop",
            input: none.clone(),
            lift: None,
            want: flags(false, false, true, false),
        },
        Case {
            name: "riskless allow on an acting browser tool",
            rules: vec![rule("mcp__browser__*", Allow, None)],
            tool: "mcp__browser__browser_click",
            input: none.clone(),
            lift: None,
            want: flags(false, false, true, false),
        },
        Case {
            name: "bash: allow with git push",
            rules: vec![rule("bash", Allow, None)],
            tool: "bash",
            input: json!({ "command": "git push origin main" }),
            lift: None,
            want: flags(false, false, true, false),
        },
        Case {
            name: "bash: allow with a local read",
            rules: vec![rule("bash", Allow, None)],
            tool: "bash",
            input: json!({ "command": "ls -la" }),
            lift: None,
            want: flags(false, false, false, false),
        },
        Case {
            name: "lift on an allow call",
            rules: vec![rule("bash", Allow, None)],
            tool: "bash",
            input: json!({ "command": "ls" }),
            lift: Some(RiskTier::Destructive),
            want: flags(false, false, false, true),
        },
        Case {
            name: "default + lift",
            rules: vec![],
            tool: "write_file",
            input: none.clone(),
            lift: Some(RiskTier::Destructive),
            want: flags(false, true, false, true),
        },
        Case {
            name: "clamp + lift",
            rules: vec![rule("bash", Allow, None)],
            tool: "bash",
            input: json!({ "command": "rm -rf build" }),
            lift: Some(RiskTier::Destructive),
            want: flags(false, false, true, true),
        },
    ]
}

#[test]
fn ask_origin_table() {
    for c in table() {
        let got = ask_origin(&c.rules, c.tool, Some(&c.input), c.lift, false);
        assert_eq!(got, c.want, "{}", c.name);
    }
}

#[test]
fn a_hook_ask_sets_hook_raised_only() {
    let got = ask_origin(&[], "write_file", None, None, true);
    assert!(got.hook_raised && got.default && !got.explicit);
}

/// The origin and the policy must not drift: some flag is set exactly when
/// the call reaches the gate as an ask.
#[test]
fn ask_origin_agrees_with_effective_call_policy() {
    for c in table() {
        let o = ask_origin(&c.rules, c.tool, Some(&c.input), c.lift, false);
        let any = o.explicit || o.default || o.clamp || o.intent_raised;
        let asks = effective_call_policy(&c.rules, c.tool, Some(&c.input)) == ToolPolicy::Ask
            || c.lift.is_some();
        assert_eq!(any, asks, "{}", c.name);
    }
}

/// `AskOrigin` lives in `mur-common` because `may_waive` takes it there; a
/// second definition in the runtime would split the type.
#[test]
fn runtime_defines_no_ask_origin() {
    fn walk(dir: &std::path::Path, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, hits);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let s = std::fs::read_to_string(&p).unwrap();
                let needles = [concat!("struct ", "AskOrigin"), concat!("enum ", "AskOrigin")];
                if needles.iter().any(|n| s.contains(n)) {
                    hits.push(p.display().to_string());
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut hits,
    );
    assert!(hits.is_empty(), "AskOrigin redefined in: {hits:?}");
}
