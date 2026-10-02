use super::hook_async_flags;
use std::collections::HashMap;

#[test]
fn claude_hooks_have_async_flags() {
    let events = [
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SessionStart",
    ];
    let mut results: HashMap<&str, serde_json::Value> = HashMap::new();
    for event_name in events {
        let (async_flag, rewake_flag) = hook_async_flags(event_name);
        let mut hook_entry = serde_json::json!({
            "hooks": [{"type": "command", "command": "bash /tmp/hook.sh"}],
            "matcher": ""
        });
        if async_flag {
            hook_entry["hooks"][0]["async"] = serde_json::json!(true);
        }
        if rewake_flag {
            hook_entry["hooks"][0]["asyncRewake"] = serde_json::json!(true);
        }
        results.insert(event_name, hook_entry);
    }
    assert_eq!(
        results["UserPromptSubmit"]["hooks"][0]["async"],
        serde_json::json!(true)
    );
    assert_eq!(
        results["Stop"]["hooks"][0]["asyncRewake"],
        serde_json::json!(true)
    );
    assert!(results["PreToolUse"]["hooks"][0].get("async").is_none());
    assert!(
        results["PostToolUse"]["hooks"][0]
            .get("asyncRewake")
            .is_none()
    );
    assert!(
        results["SessionStart"]["hooks"][0].get("async").is_none(),
        "SessionStart should not have async flag"
    );
    assert!(
        results["SessionStart"]["hooks"][0]
            .get("asyncRewake")
            .is_none(),
        "SessionStart should not have asyncRewake flag"
    );
}
