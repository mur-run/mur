use super::*;
use crate::tools::{
    bash::BASH, bash_control, edit_file::EDIT_FILE, fleet_run::FLEET_RUN, git_push,
    open_item::OPEN_ITEM, read_file::READ_FILE, recall::RECALL, remember::REMEMBER, remind::REMIND,
    suggest::SUGGEST_REPLIES, write_file::WRITE_FILE,
};
use mur_common::agent::FilesystemEntitlement;
use mur_common::proposal::PROPOSE_TOOL;
use serde_json::json;

/// Every built-in's `name()` constant, by the identifier its `name()` returns.
/// The scan below fails when a `ToolExecutor` under `src/tools/` returns an
/// identifier missing from here — which is how a new tool is forced through
/// `classify_builtin`.
const BUILTINS: &[(&str, &str)] = &[
    ("BASH", BASH),
    ("BASH_WAIT", bash_control::BASH_WAIT),
    ("BASH_KILL", bash_control::BASH_KILL),
    ("READ_FILE", READ_FILE),
    ("WRITE_FILE", WRITE_FILE),
    ("EDIT_FILE", EDIT_FILE),
    ("REMEMBER", REMEMBER),
    ("RECALL", RECALL),
    ("REMIND", REMIND),
    ("OPEN_ITEM", OPEN_ITEM),
    ("FLEET_RUN", FLEET_RUN),
    ("GIT_PUSH_REQUEST", git_push::GIT_PUSH_REQUEST),
    ("GIT_PUSH_STATUS", git_push::GIT_PUSH_STATUS),
    ("GIT_PUSH_CANCEL", git_push::GIT_PUSH_CANCEL),
    ("SUGGEST_REPLIES", SUGGEST_REPLIES),
    ("PROPOSE_TOOL", PROPOSE_TOOL),
];

/// The identifier each non-test `impl ToolExecutor for` under `src/tools/`
/// returns from `name()`. `mcp.rs` is the MCP proxy (its name is the wire
/// name, classified by leaf), not a built-in.
fn scanned_name_idents() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tools");
    let pattern = format!("{}/**/*.rs", root.display());
    let mut out = Vec::new();
    for path in glob::glob(&pattern).unwrap().flatten() {
        if path.ends_with("mcp.rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        // Inline test modules hold doubles, not built-ins. Cut at the first
        // `#[cfg(test)] mod`, not the first `#[cfg(test)]`: a test-only
        // constructor may sit above the real impl (`write_file.rs`).
        let src = src.split("#[cfg(test)]\nmod ").next().unwrap_or("");
        for chunk in src.split("impl ToolExecutor for ").skip(1) {
            let body = chunk.split("fn name(&self) -> &str {").nth(1).unwrap_or("");
            let ident = body.split('}').next().unwrap_or("").trim().to_string();
            out.push((path.display().to_string(), ident));
        }
    }
    out
}

#[test]
fn every_builtin_tool_is_classified() {
    let scanned = scanned_name_idents();
    assert!(
        scanned.len() >= BUILTINS.len(),
        "scan found {} executors, fewer than the {} known — the scan broke",
        scanned.len(),
        BUILTINS.len()
    );
    for (file, ident) in &scanned {
        assert!(
            BUILTINS.iter().any(|(i, _)| i == ident),
            "{file}: `name()` returns `{ident}`, which B0 does not know. Add it \
             to BUILTINS here and classify it in `tool_map::classify_builtin` \
             (#2181) — an unclassified side-effect tool skips Rule 4."
        );
    }
    for (ident, name) in BUILTINS {
        assert!(
            classify_builtin(name).is_some(),
            "`{ident}` (`{name}`) is not in classify_builtin"
        );
    }
}

#[test]
fn side_effect_builtins_are_gated_by_rule_4() {
    for name in [
        BASH,
        bash_control::BASH_KILL,
        WRITE_FILE,
        EDIT_FILE,
        REMEMBER,
        REMIND,
        OPEN_ITEM,
        FLEET_RUN,
        git_push::GIT_PUSH_REQUEST,
        git_push::GIT_PUSH_CANCEL,
    ] {
        assert!(is_side_effect_tool(name), "{name}");
    }
    for name in [
        READ_FILE,
        RECALL,
        SUGGEST_REPLIES,
        PROPOSE_TOOL,
        git_push::GIT_PUSH_STATUS,
        bash_control::BASH_WAIT,
    ] {
        assert!(!is_side_effect_tool(name), "{name}");
    }
}

#[test]
fn mcp_tools_classify_by_leaf() {
    for name in [
        "mcp__mur-tools__parallel_jobs",
        "mcp__browser__browser_navigate",
        "mcp__playwright__browser_click",
        "mcp__slack__send_message",
    ] {
        assert!(is_side_effect_tool(name), "{name}");
    }
    for name in [
        "mcp__browser__browser_snapshot",
        "mcp__mur-tools__mur_job_status",
        "mcp__mur-commander__find_symbol",
    ] {
        assert!(!is_side_effect_tool(name), "{name}");
    }
    // The legacy dotted names still classify (hook consumers, B0 corpus).
    assert!(is_side_effect_tool("messaging.send"));
    assert!(!is_side_effect_tool("fs.read"));
}

fn call(name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall::test(name, input)
}

#[test]
fn rule_2_reads_url_from_namespaced_mcp_tools() {
    let nav = call(
        "mcp__browser__browser_navigate",
        json!({"url": "https://evil.example.com/x"}),
    );
    assert_eq!(outbound_url(&nav), Some(Some("https://evil.example.com/x")));
    // Any server prefix: the leaf decides.
    let nav = call(
        "mcp__pw__browser_navigate",
        json!({"url": "https://a.test"}),
    );
    assert_eq!(outbound_url(&nav), Some(Some("https://a.test")));
    // `browser_tabs list` names no URL: not a network call.
    let tabs = call("mcp__browser__browser_tabs", json!({"action": "list"}));
    assert_eq!(outbound_url(&tabs), None);
    // Not a network tool.
    assert_eq!(outbound_url(&call(WRITE_FILE, json!({"path": "/x"}))), None);
    // Legacy names keep their gate, URL or not.
    assert_eq!(
        outbound_url(&call("network.http_get", json!({}))),
        Some(None)
    );
}

#[test]
fn leaf_strips_only_the_mcp_prefix() {
    assert_eq!(leaf("mcp__browser__browser_navigate"), "browser_navigate");
    assert_eq!(leaf("mcp__a__b__c"), "b__c");
    assert_eq!(leaf("write_file"), "write_file");
}

/// `<tmp>/agents/me`, canonical, with a sibling `<tmp>/agents/other`.
fn mur_home() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("agents/me");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(root.join("agents/other")).unwrap();
    (tmp, home)
}

fn path_call(name: &str, p: &Path) -> ToolCall {
    call(
        name,
        json!({"path": p.display().to_string(), "content": "x"}),
    )
}

#[test]
fn rule_1_refuses_the_launch_chain_and_deny_roots() {
    let (_tmp, home) = mur_home();
    let other = home.parent().unwrap().join("other/profile.yaml");
    let secret = home.parent().unwrap().parent().unwrap().join("vault");
    std::fs::create_dir_all(&secret).unwrap();
    let fs = FilesystemEntitlement {
        deny: vec![secret.display().to_string()],
        ..Default::default()
    };

    for name in [WRITE_FILE, EDIT_FILE] {
        let (r, _) = hard_refused_write(&path_call(name, &other), &home, &fs)
            .unwrap_or_else(|| panic!("{name}: another agent's profile"));
        assert_eq!(r.code(), "launch_chain");

        let (r, _) = hard_refused_write(&path_call(name, &secret.join("k")), &home, &fs)
            .unwrap_or_else(|| panic!("{name}: deny root"));
        assert_eq!(r.code(), "fs_deny");

        // The agent's own profile is a self-protected deny (#712).
        let (r, _) = hard_refused_write(&path_call(name, &home.join("profile.yaml")), &home, &fs)
            .unwrap_or_else(|| panic!("{name}: own profile"));
        assert_eq!(r.code(), "fs_deny");
    }
}

#[test]
fn rule_1_leaves_ordinary_writes_to_the_tool() {
    let (_tmp, home) = mur_home();
    let fs = FilesystemEntitlement::default();
    let outside = tempfile::tempdir().unwrap();
    for (name, p) in [
        (WRITE_FILE, home.join("notes.md")),
        // Outside the home and ungranted: the tool refuses it, not B0.
        (WRITE_FILE, outside.path().join("project/src/main.rs")),
        (EDIT_FILE, outside.path().join("a.rs")),
    ] {
        assert!(
            hard_refused_write(&path_call(name, &p), &home, &fs).is_none(),
            "{name} {}",
            p.display()
        );
    }
    // Relative: resolves against a session cwd B0 does not see — skipped.
    assert!(
        hard_refused_write(
            &call(WRITE_FILE, json!({"path": "profile.yaml"})),
            &home,
            &fs
        )
        .is_none()
    );
    // Not a write tool.
    let other = home.parent().unwrap().join("other/profile.yaml");
    assert!(hard_refused_write(&path_call(READ_FILE, &other), &home, &fs).is_none());
}
