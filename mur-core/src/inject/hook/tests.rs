use super::*;
use mur_common::knowledge::Content;
use mur_common::knowledge::KnowledgeBase;
use mur_common::workflow::Step;

fn make_workflow(desc: &str) -> Workflow {
    Workflow {
        base: KnowledgeBase {
            name: "test-wf".into(),
            description: desc.into(),
            content: Content::Plain("workflow content".into()),
            ..Default::default()
        },
        steps: vec![Step {
            order: 1,
            description: "Run tests".into(),
            command: Some("cargo test".into()),
            tool: Some("cargo".into()),
            ..Default::default()
        }],
        variables: vec![],
        source_sessions: vec![],
        trigger: String::new(),
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    }
}

#[test]
fn test_detect_error_trigger() {
    assert_eq!(
        detect_trigger("I got an error: cannot find module"),
        HookTrigger::OnError
    );
    assert_eq!(
        detect_trigger("Build failed with exit code 1"),
        HookTrigger::OnError
    );
    assert_eq!(detect_trigger("panic at thread main"), HookTrigger::OnError);
    assert_eq!(detect_trigger("程式崩潰了"), HookTrigger::OnError);
}

#[test]
fn test_detect_retry_trigger() {
    assert_eq!(detect_trigger("try again please"), HookTrigger::OnRetry);
    assert_eq!(detect_trigger("retry the build"), HookTrigger::OnRetry);
    assert_eq!(detect_trigger("還是不行"), HookTrigger::OnRetry);
}

#[test]
fn test_detect_session_start() {
    assert_eq!(
        detect_trigger("Build a REST API for users"),
        HookTrigger::SessionStart
    );
    assert_eq!(
        detect_trigger("Refactor the auth module"),
        HookTrigger::SessionStart
    );
}

#[test]
fn test_format_workflow_entry() {
    let wf = make_workflow("Deploy to production");
    let entry = format_workflow_entry(&wf, 1);
    assert!(entry.contains("[Workflow:"));
    assert!(entry.contains("Deploy to production"));
    assert!(entry.contains("cargo test"));
    assert!(entry.contains("Run tests"));
}
