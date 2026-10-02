use super::*;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use mur_common::workflow::Step;

fn make_pattern(desc: &str, content: &str) -> Pattern {
    Pattern {
        base: KnowledgeBase {
            schema: 2,
            name: "test".into(),
            description: desc.into(),
            content: Content::Plain(content.into()),
            tier: Tier::Session,
            importance: 0.5,
            confidence: 0.5,
            tags: Tags::default(),
            applies: Applies::default(),
            evidence: Evidence::default(),
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    }
}

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
fn test_empty_patterns() {
    assert_eq!(format_for_injection_with_store(&[], 2000, None), "");
}

#[test]
fn test_single_pattern() {
    let p = make_pattern("Use Swift Testing", "Use @Test macro");
    let result = format_for_injection_with_store(&[p], 2000, None);
    assert!(result.contains("Use Swift Testing"));
    assert!(result.contains("@Test macro"));
}

#[test]
fn test_dual_layer() {
    let p = Pattern {
        base: KnowledgeBase {
            schema: 2,
            name: "test".into(),
            description: "Test".into(),
            content: Content::DualLayer {
                technical: "Do X".into(),
                principle: Some("Because Y".into()),
            },
            tier: Tier::Session,
            importance: 0.5,
            confidence: 0.5,
            tags: Tags::default(),
            applies: Applies::default(),
            evidence: Evidence::default(),
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    };
    let result = format_for_injection_with_store(&[p], 2000, None);
    assert!(result.contains("Do X"));
    assert!(result.contains("💡 Because Y"));
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
fn test_token_budget() {
    let patterns: Vec<Pattern> = (0..20)
        .map(|i| make_pattern(&format!("Pattern {}", i), &"x".repeat(500)))
        .collect();
    let result = format_for_injection_with_store(&patterns, 500, None);
    // Should not include all 20 patterns
    let count = result.matches("###").count();
    assert!(count < 20);
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

// ─── Phase 3: Diagram attachment injection tests ────────────

#[test]
fn test_injection_with_diagram_attachment_no_store() {
    let p = Pattern {
        base: KnowledgeBase {
            schema: 2,
            name: "arch-pattern".into(),
            description: "Architecture pattern".into(),
            content: Content::Plain("Use microservices.".into()),
            tier: Tier::Core,
            importance: 0.8,
            confidence: 0.9,
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![Attachment {
            att_type: AttachmentType::Diagram,
            format: AttachmentFormat::Mermaid,
            path: "arch-pattern/overview.mermaid".into(),
            description: "System architecture".into(),
        }],
    };

    // Without store, diagram can't be resolved — should show path fallback
    let result = format_for_injection_with_store(&[p], 5000, None);
    assert!(result.contains("Architecture pattern"));
    assert!(result.contains("Use microservices"));
    assert!(result.contains("Diagram: System architecture"));
    assert!(result.contains("arch-pattern/overview.mermaid"));
}

#[test]
fn test_injection_with_diagram_attachment_with_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = crate::store::yaml::YamlStore::new(tmp.path().to_path_buf()).unwrap();

    // Create the diagram file
    let assets_dir = tmp.path().join("arch-pattern");
    std::fs::create_dir_all(&assets_dir).unwrap();
    std::fs::write(
        assets_dir.join("overview.mermaid"),
        "graph TD\n    A[Client]-->B[Server]\n    B-->C[Database]",
    )
    .unwrap();

    let p = Pattern {
        base: KnowledgeBase {
            schema: 2,
            name: "arch-pattern".into(),
            description: "Architecture pattern".into(),
            content: Content::Plain("Use microservices.".into()),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![Attachment {
            att_type: AttachmentType::Diagram,
            format: AttachmentFormat::Mermaid,
            path: "arch-pattern/overview.mermaid".into(),
            description: "System architecture".into(),
        }],
    };

    let result = format_for_injection_with_store(&[p], 5000, Some(&store));
    assert!(result.contains("## Diagram: System architecture"));
    assert!(result.contains("```mermaid"));
    assert!(result.contains("A[Client]-->B[Server]"));
    assert!(result.contains("```"));
}

#[test]
fn test_injection_image_attachment_description_only() {
    let p = Pattern {
        base: KnowledgeBase {
            schema: 2,
            name: "ui-pattern".into(),
            description: "UI pattern".into(),
            content: Content::Plain("Use dark theme.".into()),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![Attachment {
            att_type: AttachmentType::Image,
            format: AttachmentFormat::Png,
            path: "ui-pattern/screenshot.png".into(),
            description: "Dark mode screenshot".into(),
        }],
    };

    let result = format_for_injection_with_store(&[p], 5000, None);
    assert!(result.contains("Dark mode screenshot"));
    // Should NOT contain mermaid code fence
    assert!(!result.contains("```mermaid"));
    assert!(!result.contains("```png"));
}

#[test]
fn test_pattern_without_attachments_unchanged() {
    let p = make_pattern("No attachments", "Use foo bar.");
    let result = format_for_injection_with_store(&[p], 5000, None);
    assert!(result.contains("No attachments"));
    assert!(result.contains("Use foo bar"));
    // No attachment markers
    assert!(!result.contains("Diagram:"));
    assert!(!result.contains("📎"));
}

// ─── Kind-aware formatting tests ────────────────────────────

#[test]
fn test_mixed_kind_injection_grouped() {
    let mut p_pref = make_pattern("Prefer Chinese", "Always use Traditional Chinese");
    p_pref.kind = Some(PatternKind::Preference);

    let mut p_proc = make_pattern("Deploy steps", "1. Run tests 2. Build 3. Deploy");
    p_proc.kind = Some(PatternKind::Procedure);

    let p_tech = make_pattern("Use @Test", "Use @Test macro for Swift testing");
    // kind is None = Technical

    let result = format_for_injection_with_store(&[p_pref, p_proc, p_tech], 5000, None);
    assert!(
        result.contains("User Preferences"),
        "Should have Preferences header"
    );
    assert!(
        result.contains("Procedures"),
        "Should have Procedures header"
    );
    assert!(result.contains("Knowledge"), "Should have Knowledge header");
    assert!(result.contains("Traditional Chinese"));
    assert!(result.contains("Deploy steps"));
    assert!(result.contains("@Test macro"));
}

#[test]
fn test_all_technical_uses_flat_format() {
    let p1 = make_pattern("Pattern A", "Content A");
    let p2 = make_pattern("Pattern B", "Content B");
    let result = format_for_injection_with_store(&[p1, p2], 5000, None);
    // Should use the old flat format header
    assert!(result.contains("Relevant patterns from your learning history"));
    // Should NOT have kind-group headers
    assert!(!result.contains("User Preferences"));
    assert!(!result.contains("Procedures"));
}

#[test]
fn test_explicit_technical_kind_still_flat() {
    // explicit kind=Technical should still use flat format
    let mut p1 = make_pattern("Pattern A", "Content A");
    p1.kind = Some(PatternKind::Technical);
    let mut p2 = make_pattern("Pattern B", "Content B");
    p2.kind = Some(PatternKind::Technical);
    let result = format_for_injection_with_store(&[p1, p2], 5000, None);
    assert!(result.contains("Relevant patterns from your learning history"));
    assert!(!result.contains("Knowledge"));
}

#[test]
fn test_explicit_fact_kind_triggers_grouped_format() {
    // Fact with explicit kind → grouped (even though it's in Knowledge group)
    let mut p1 = make_pattern("Server address", "prod.example.com:8080");
    p1.kind = Some(PatternKind::Fact);
    let result = format_for_injection_with_store(&[p1], 5000, None);
    assert!(
        result.contains("Relevant knowledge from your learning history"),
        "Explicit Fact kind should use grouped header"
    );
    assert!(result.contains("Knowledge"));
}

#[test]
fn test_preferences_as_bullet_list() {
    let mut p = make_pattern("Short responses", "Keep answers concise");
    p.kind = Some(PatternKind::Preference);

    let mut p2 = make_pattern("Tech pattern", "Use Rust");
    p2.kind = Some(PatternKind::Technical);

    let result = format_for_injection_with_store(&[p, p2], 5000, None);
    // Preferences should be bullet points
    assert!(result.contains("- **"));
}

// ─── Token-budget edge cases for grouped injection ───────────

#[test]
fn test_grouped_injection_no_orphaned_headers() {
    // Tight budget: fits one preference but NOT the procedures header+entry.
    // We should NOT see an empty "## Procedures" section in the output.
    let mut pref = make_pattern("Keep it short", "Use terse replies.");
    pref.kind = Some(PatternKind::Preference);

    // A large procedure that cannot fit after the preference.
    let mut proc_pattern = make_pattern("Big procedure", &"step ".repeat(800));
    proc_pattern.kind = Some(PatternKind::Procedure);

    // Token budget: ~120 tokens (≈480 chars).
    // The preference entry is small; the procedure entry is huge.
    let result = format_for_injection_with_store(&[pref, proc_pattern], 120, None);

    // Preference should be present
    assert!(
        result.contains("Keep it short"),
        "preference should be included"
    );
    // The Procedures header must NOT appear without any content under it
    if result.contains("Procedures") {
        // If header appears, at least one procedure entry must follow it
        let proc_header_pos = result.find("## Procedures").unwrap();
        let after_header = &result[proc_header_pos..];
        assert!(
            after_header.contains("Big procedure"),
            "Procedures header present but no entries — orphaned header"
        );
    }
}
