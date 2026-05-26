use mur_common::skill::manifest::ProcedureStep;
use mur_common::skill::mcp::{McpRequirement, SkillCapability};
use mur_common::skill::{resolve_step, McpInventory, Resolution};

fn step(tool: Option<&str>, intent: Option<&str>, hint: Option<&str>) -> ProcedureStep {
    ProcedureStep {
        description: "test".into(),
        tool: tool.map(String::from),
        intent: intent.map(String::from),
        tool_hint: hint.map(String::from),
    }
}

// ── Rule 1: literal tool, no intent ──

#[test]
fn literal_only_unchanged() {
    let r = resolve_step(
        &step(Some("browser.navigate"), None, None),
        &[],
        &McpInventory::default(),
    );
    assert_eq!(
        r,
        Resolution::Literal {
            tool: "browser.navigate".into()
        }
    );
}

#[test]
fn literal_tool_with_intent_still_triggers_resolution() {
    // When intent IS set, the literal tool is not returned as Literal.
    let inv = McpInventory::from_tool_names(vec!["browser.navigate".into()]);
    let r = resolve_step(
        &step(Some("old.tool"), Some("web_navigate"), None),
        &[],
        &inv,
    );
    // No requirements → no glob match → unresolved (Rule 5).
    assert!(matches!(r, Resolution::Unresolved { .. }));
}

#[test]
fn neither_tool_nor_intent_is_unresolved() {
    let r = resolve_step(&step(None, None, None), &[], &McpInventory::default());
    assert!(matches!(r, Resolution::Unresolved { .. }));
}

// ── Rule 2: tool_hint ──

#[test]
fn tool_hint_literal_matches() {
    let inv = McpInventory::from_tool_names(vec!["browser.navigate".into()]);
    let r = resolve_step(
        &step(None, Some("web_navigate"), Some("browser.navigate")),
        &[],
        &inv,
    );
    assert_eq!(
        r,
        Resolution::Hint {
            tool: "browser.navigate".into()
        }
    );
}

#[test]
fn tool_hint_glob_picks_shortest() {
    let inv = McpInventory::from_tool_names(vec![
        "browser.navigate.full".into(),
        "browser.navigate".into(),
        "browser.click".into(),
    ]);
    let r = resolve_step(
        &step(None, Some("web_navigate"), Some("browser.*")),
        &[],
        &inv,
    );
    // "browser.click" (13) and "browser.navigate" (16) — shortest is "browser.click"
    assert_eq!(
        r,
        Resolution::Hint {
            tool: "browser.click".into()
        }
    );
}

#[test]
fn tool_hint_not_found_falls_through() {
    let inv = McpInventory::from_tool_names(vec!["other.tool".into()]);
    let r = resolve_step(
        &step(None, Some("web_navigate"), Some("browser.navigate")),
        &[],
        &inv,
    );
    // Hint didn't match, no requirements → unresolved.
    assert!(matches!(r, Resolution::Unresolved { .. }));
}

// ── Rule 3: intent_match ──

#[test]
fn intent_match_picks_shortest_glob_hit() {
    let inv = McpInventory::from_tool_names(vec![
        "browser.navigate.full_page".into(),
        "browser.navigate".into(),
        "browser.click".into(),
    ]);
    let reqs = vec![McpRequirement {
        tool_pattern: "browser.*".into(),
        capability: SkillCapability::NetworkHttp,
        fallback: String::new(),
    }];
    let r = resolve_step(&step(None, Some("web_navigate"), None), &reqs, &inv);
    assert_eq!(
        r,
        Resolution::IntentMatch {
            tool: "browser.click".into(),
            capability: SkillCapability::NetworkHttp,
        }
    );
}

#[test]
fn intent_match_first_requirement_wins() {
    let inv = McpInventory::from_tool_names(vec!["fs.read".into(), "browser.navigate".into()]);
    let reqs = vec![
        McpRequirement {
            tool_pattern: "fs.*".into(),
            capability: SkillCapability::ReadFile,
            fallback: String::new(),
        },
        McpRequirement {
            tool_pattern: "browser.*".into(),
            capability: SkillCapability::NetworkHttp,
            fallback: String::new(),
        },
    ];
    let r = resolve_step(&step(None, Some("read"), None), &reqs, &inv);
    assert_eq!(
        r,
        Resolution::IntentMatch {
            tool: "fs.read".into(),
            capability: SkillCapability::ReadFile,
        }
    );
}

#[test]
fn intent_match_skips_invalid_glob() {
    let inv = McpInventory::from_tool_names(vec!["fs.read".into()]);
    let reqs = vec![
        McpRequirement {
            tool_pattern: "[invalid".into(),
            capability: SkillCapability::ReadFile,
            fallback: String::new(),
        },
        McpRequirement {
            tool_pattern: "fs.*".into(),
            capability: SkillCapability::WriteFile,
            fallback: String::new(),
        },
    ];
    let r = resolve_step(&step(None, Some("write"), None), &reqs, &inv);
    // First requirement's glob is invalid → skipped. Second matches.
    assert_eq!(
        r,
        Resolution::IntentMatch {
            tool: "fs.read".into(),
            capability: SkillCapability::WriteFile,
        }
    );
}

#[test]
fn intent_match_no_glob_match_goes_to_fallback() {
    let inv = McpInventory::from_tool_names(vec!["builtin-http".into()]);
    let reqs = vec![McpRequirement {
        tool_pattern: "browser.*".into(),
        capability: SkillCapability::NetworkHttp,
        fallback: "builtin-http".into(),
    }];
    let r = resolve_step(&step(None, Some("web_navigate"), None), &reqs, &inv);
    // No browser.* tool in inventory, but fallback matches.
    assert_eq!(
        r,
        Resolution::Fallback {
            tool: "builtin-http".into(),
            capability: SkillCapability::NetworkHttp,
        }
    );
}

// ── Rule 4: fallback ──

#[test]
fn fallback_used_when_no_glob_match() {
    let inv = McpInventory::from_tool_names(vec!["builtin-http".into(), "other.tool".into()]);
    let reqs = vec![McpRequirement {
        tool_pattern: "browser.*".into(),
        capability: SkillCapability::NetworkHttp,
        fallback: "builtin-http".into(),
    }];
    let r = resolve_step(&step(None, Some("web_navigate"), None), &reqs, &inv);
    assert_eq!(
        r,
        Resolution::Fallback {
            tool: "builtin-http".into(),
            capability: SkillCapability::NetworkHttp,
        }
    );
}

#[test]
fn fallback_skipped_when_not_in_inventory() {
    let inv = McpInventory::from_tool_names(vec!["other.tool".into()]);
    let reqs = vec![McpRequirement {
        tool_pattern: "browser.*".into(),
        capability: SkillCapability::NetworkHttp,
        fallback: "missing-fallback".into(),
    }];
    let r = resolve_step(&step(None, Some("web_navigate"), None), &reqs, &inv);
    // No glob match, fallback not in inventory → unresolved.
    assert!(matches!(r, Resolution::Unresolved { .. }));
}

// ── Rule 5: unresolved ──

#[test]
fn unresolved_when_nothing_matches() {
    let inv = McpInventory::from_tool_names(vec!["other.tool".into()]);
    let reqs = vec![McpRequirement {
        tool_pattern: "browser.*".into(),
        capability: SkillCapability::NetworkHttp,
        fallback: String::new(),
    }];
    let r = resolve_step(&step(None, Some("web_navigate"), None), &reqs, &inv);
    assert!(matches!(r, Resolution::Unresolved { .. }));
}

// ── Source tag ──

#[test]
fn source_tags_are_correct() {
    assert_eq!(
        Resolution::Literal {
            tool: "x".into()
        }
        .source_tag(),
        "literal"
    );
    assert_eq!(
        Resolution::Hint { tool: "x".into() }.source_tag(),
        "hint"
    );
    assert_eq!(
        Resolution::IntentMatch {
            tool: "x".into(),
            capability: SkillCapability::ReadFile
        }
        .source_tag(),
        "intent_match"
    );
    assert_eq!(
        Resolution::Fallback {
            tool: "x".into(),
            capability: SkillCapability::ReadFile
        }
        .source_tag(),
        "fallback"
    );
    assert_eq!(
        Resolution::Unresolved {
            reason: "nope".into()
        }
        .source_tag(),
        "unresolved"
    );
}

// ── picked_tool ──

#[test]
fn picked_tool_returns_tool_name() {
    assert_eq!(
        Resolution::Literal {
            tool: "a".into()
        }
        .picked_tool(),
        Some("a")
    );
    assert_eq!(
        Resolution::Unresolved {
            reason: "nope".into()
        }
        .picked_tool(),
        None
    );
}

// ── hint beats intent-match ──

#[test]
fn tool_hint_takes_priority_over_intent_match() {
    let inv = McpInventory::from_tool_names(vec![
        "browser.navigate".into(),
        "browser.click".into(),
    ]);
    let reqs = vec![McpRequirement {
        tool_pattern: "browser.*".into(),
        capability: SkillCapability::NetworkHttp,
        fallback: String::new(),
    }];
    // hint matches first (Rule 2), so we never reach Rule 3 (intent_match).
    let r = resolve_step(
        &step(None, Some("web_navigate"), Some("browser.navigate")),
        &reqs,
        &inv,
    );
    assert_eq!(
        r,
        Resolution::Hint {
            tool: "browser.navigate".into()
        }
    );
}
