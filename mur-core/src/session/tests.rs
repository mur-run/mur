use super::*;

#[test]
fn test_active_session_roundtrip() {
    let session = ActiveSession {
        id: "test-123".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        source: "claude-code".to_string(),
    };

    let json = serde_json::to_string_pretty(&session).unwrap();
    let parsed: ActiveSession = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id, "test-123");
    assert_eq!(parsed.source, "claude-code");
    assert_eq!(parsed.started_at, "2026-01-01T00:00:00Z");
}

#[test]
fn test_session_file_operations() {
    let tmp = tempfile::TempDir::new().unwrap();
    let session_dir = tmp.path().join("session");
    let recordings_dir = session_dir.join("recordings");
    fs::create_dir_all(&recordings_dir).unwrap();

    // Write active session
    let session = ActiveSession {
        id: "abc-123".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        source: "test".to_string(),
    };
    let active_path = session_dir.join("active.json");
    fs::write(
        &active_path,
        serde_json::to_string_pretty(&session).unwrap(),
    )
    .unwrap();

    // Verify it can be read back
    let content = fs::read_to_string(&active_path).unwrap();
    let parsed: ActiveSession = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.id, "abc-123");

    // Write events to JSONL
    let recording_path = recordings_dir.join("abc-123.jsonl");
    let events = vec![
        SessionEvent {
            timestamp: 1000,
            event_type: "user".to_string(),
            tool: None,
            content: "hello".to_string(),
            ..Default::default()
        },
        SessionEvent {
            timestamp: 2000,
            event_type: "assistant".to_string(),
            tool: None,
            content: "hi".to_string(),
            ..Default::default()
        },
        SessionEvent {
            timestamp: 3000,
            event_type: "tool_call".to_string(),
            tool: Some("Bash".to_string()),
            content: "ls".to_string(),
            ..Default::default()
        },
    ];

    let mut file = fs::File::create(&recording_path).unwrap();
    for event in &events {
        let mut line = serde_json::to_string(event).unwrap();
        line.push('\n');
        file.write_all(line.as_bytes()).unwrap();
    }

    // Read back and verify
    let content = fs::read_to_string(&recording_path).unwrap();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3);

    let first: SessionEvent = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first.event_type, "user");
    assert_eq!(first.content, "hello");

    let third: SessionEvent = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(third.event_type, "tool_call");
    assert_eq!(third.tool.as_deref(), Some("Bash"));

    // Clean up (stop)
    fs::remove_file(&active_path).unwrap();
    assert!(!active_path.exists());
}

#[test]
fn test_session_event_serialization() {
    let event = SessionEvent {
        timestamp: 1708848000000,
        event_type: "user".to_string(),
        tool: None,
        content: "hello world".to_string(),
        ..Default::default()
    };

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"user\""));
    assert!(!json.contains("\"tool\""));

    let event_with_tool = SessionEvent {
        timestamp: 1708848000000,
        event_type: "tool_call".to_string(),
        tool: Some("Bash".to_string()),
        content: "ls -la".to_string(),
        ..Default::default()
    };

    let json = serde_json::to_string(&event_with_tool).unwrap();
    assert!(json.contains("\"tool\":\"Bash\""));
}

#[test]
fn test_session_event_deserialization() {
    let json =
        r#"{"timestamp":1708848000000,"type":"tool_call","tool":"Read","content":"file.rs"}"#;
    let event: SessionEvent = serde_json::from_str(json).unwrap();
    assert_eq!(event.event_type, "tool_call");
    assert_eq!(event.tool.as_deref(), Some("Read"));
    assert_eq!(event.content, "file.rs");
}

#[test]
fn test_jsonl_append_format() {
    let events = vec![
        SessionEvent {
            timestamp: 1000,
            event_type: "user".to_string(),
            tool: None,
            content: "first".to_string(),
            ..Default::default()
        },
        SessionEvent {
            timestamp: 2000,
            event_type: "assistant".to_string(),
            tool: None,
            content: "second".to_string(),
            ..Default::default()
        },
    ];

    let mut buf = String::new();
    for event in &events {
        let mut line = serde_json::to_string(event).unwrap();
        line.push('\n');
        buf.push_str(&line);
    }

    let lines: Vec<&str> = buf.lines().collect();
    assert_eq!(lines.len(), 2);

    // Each line should be valid JSON
    for line in &lines {
        let _: SessionEvent = serde_json::from_str(line).unwrap();
    }
}

#[test]
fn test_recording_info_sorting() {
    use std::time::{Duration, SystemTime};

    let mut recordings = [
        RecordingInfo {
            id: "old".to_string(),
            event_count: 5,
            file_size: 100,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1000),
            meta: None,
        },
        RecordingInfo {
            id: "new".to_string(),
            event_count: 10,
            file_size: 200,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(2000),
            meta: None,
        },
    ];

    recordings.sort_by_key(|r| std::cmp::Reverse(r.modified));
    assert_eq!(recordings[0].id, "new");
    assert_eq!(recordings[1].id, "old");
}

// ─── should_skip tests ─────────────────────────────────────────

#[test]
fn test_should_skip_empty_assistant() {
    assert!(should_skip("assistant", ""));
    assert!(should_skip("assistant", "   "));
}

#[test]
fn test_should_skip_stop_markers() {
    assert!(should_skip("assistant", "[stop: turn_end]"));
    assert!(should_skip("assistant", "[stop: end_turn]"));
    assert!(should_skip("assistant", "[stop: something_else]"));
}

#[test]
fn test_should_not_skip_real_assistant() {
    assert!(!should_skip("assistant", "Here is your answer"));
    assert!(!should_skip("assistant", "I'll help with that"));
}

#[test]
fn test_should_skip_mur_session_tool_call() {
    let content = r#"{"command": "mur session start"}"#;
    assert!(should_skip("tool_call", content));

    let content = r#"{"command": "mur session stop"}"#;
    assert!(should_skip("tool_call", content));
}

#[test]
fn test_should_not_skip_other_tool_calls() {
    let content = r#"{"command": "ls -la"}"#;
    assert!(!should_skip("tool_call", content));

    assert!(!should_skip("tool_call", "plain text"));
}

#[test]
fn test_should_not_skip_user_events() {
    assert!(!should_skip("user", ""));
    assert!(!should_skip("user", "[stop: turn_end]"));
}

// ─── SessionMeta tests ─────────────────────────────────────────

#[test]
fn test_session_meta_serialization() {
    let meta = SessionMeta {
        id: "test-123".to_string(),
        source: "claude-code".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        stopped_at: None,
        title: Some("Hello world".to_string()),
        tools_used: vec!["Bash".to_string(), "Read".to_string()],
        user_turns: 3,
        assistant_turns: 4,
        marked: false,
        gated_at: None,
        harvested_at: None,
    };

    let json = serde_json::to_string_pretty(&meta).unwrap();
    let parsed: SessionMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id, "test-123");
    assert_eq!(parsed.source, "claude-code");
    assert_eq!(parsed.title, Some("Hello world".to_string()));
    assert_eq!(parsed.tools_used.len(), 2);
    assert_eq!(parsed.user_turns, 3);
    assert_eq!(parsed.assistant_turns, 4);
    assert!(parsed.stopped_at.is_none());
}

#[test]
fn test_session_meta_file_operations() {
    let tmp = tempfile::TempDir::new().unwrap();
    let meta_dir = tmp.path().join("meta_test");
    fs::create_dir_all(&meta_dir).unwrap();

    let meta = SessionMeta {
        id: "abc-456".to_string(),
        source: "test".to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        stopped_at: None,
        title: None,
        tools_used: vec![],
        user_turns: 0,
        assistant_turns: 0,
        marked: false,
        gated_at: None,
        harvested_at: None,
    };

    // Write and read back
    let path = meta_dir.join("abc-456.meta.json");
    let json = serde_json::to_string_pretty(&meta).unwrap();
    fs::write(&path, &json).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: SessionMeta = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.id, "abc-456");
    assert!(parsed.title.is_none());
    assert_eq!(parsed.user_turns, 0);

    // Simulate title update
    let mut updated = parsed;
    updated.title = Some("My first session".to_string());
    updated.user_turns = 2;
    updated.tools_used = vec!["Bash".to_string()];

    let json = serde_json::to_string_pretty(&updated).unwrap();
    fs::write(&path, &json).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let final_meta: SessionMeta = serde_json::from_str(&content).unwrap();
    assert_eq!(final_meta.title, Some("My first session".to_string()));
    assert_eq!(final_meta.user_turns, 2);
    assert_eq!(final_meta.tools_used, vec!["Bash".to_string()]);
}

#[test]
fn test_title_truncation() {
    let long_content = "a".repeat(200);
    let title: String = long_content.chars().take(80).collect();
    assert_eq!(title.len(), 80);
}

#[test]
fn test_tools_used_deduplication() {
    let mut tools: BTreeSet<String> = BTreeSet::new();
    tools.insert("Bash".to_string());
    tools.insert("Read".to_string());
    tools.insert("Bash".to_string()); // duplicate
    assert_eq!(tools.len(), 2);
}

// ─── remove_recording tests ─────────────────────────────────────

#[test]
fn test_remove_recording_cleans_all_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let rec_dir = tmp.path().join("recordings");
    fs::create_dir_all(&rec_dir).unwrap();

    let id = "test-remove-123";
    // Create the three files
    fs::write(rec_dir.join(format!("{}.jsonl", id)), "line1\n").unwrap();
    fs::write(rec_dir.join(format!("{}.meta.json", id)), "{}").unwrap();
    fs::write(rec_dir.join(format!("{}.synced", id)), "2026-01-01").unwrap();

    assert!(rec_dir.join(format!("{}.jsonl", id)).exists());
    assert!(rec_dir.join(format!("{}.meta.json", id)).exists());
    assert!(rec_dir.join(format!("{}.synced", id)).exists());

    // Call the helper with explicit dir
    remove_recording_in_dir(&rec_dir, id).unwrap();

    assert!(!rec_dir.join(format!("{}.jsonl", id)).exists());
    assert!(!rec_dir.join(format!("{}.meta.json", id)).exists());
    assert!(!rec_dir.join(format!("{}.synced", id)).exists());
}

#[test]
fn gc_removes_old_unmarked_keeps_recent_and_marked() {
    let tmp = tempfile::TempDir::new().unwrap();
    let rec_dir = tmp.path().join("recordings");
    fs::create_dir_all(&rec_dir).unwrap();

    let write = |id: &str, started_days_ago: i64, marked: bool, harvested: bool| {
        fs::write(rec_dir.join(format!("{}.jsonl", id)), "{}\n").unwrap();
        let meta = SessionMeta {
            id: id.to_string(),
            source: "claude".to_string(),
            started_at: (chrono::Utc::now() - chrono::Duration::days(started_days_ago))
                .to_rfc3339(),
            stopped_at: None,
            title: None,
            tools_used: vec![],
            user_turns: 0,
            assistant_turns: 0,
            marked,
            gated_at: None,
            harvested_at: harvested.then(|| chrono::Utc::now().to_rfc3339()),
        };
        fs::write(
            rec_dir.join(format!("{}.meta.json", id)),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    };

    write("old-plain", 30, false, false); // old, unmarked → removed
    write("old-marked", 30, true, false); // old but marked, never harvested → kept
    write("old-marked-done", 30, true, true); // old, marked, harvested → removed
    write("fresh", 1, false, false); // recent → kept

    let removed = gc_in_dir(&rec_dir, 14).unwrap();
    assert_eq!(removed, 2);
    assert!(!rec_dir.join("old-plain.jsonl").exists());
    assert!(rec_dir.join("old-marked.jsonl").exists());
    assert!(!rec_dir.join("old-marked-done.jsonl").exists());
    assert!(rec_dir.join("fresh.jsonl").exists());
}

#[test]
fn meta_back_compat_without_new_fields() {
    // Old meta files (pre-ambient) must keep parsing.
    let json = r#"{"id":"x","source":"claude","started_at":"2026-01-01T00:00:00Z",
            "stopped_at":null,"title":null,"tools_used":[],"user_turns":0,"assistant_turns":0}"#;
    let meta: SessionMeta = serde_json::from_str(json).unwrap();
    assert!(!meta.marked);
    assert!(meta.gated_at.is_none());
    assert!(meta.harvested_at.is_none());
}

#[test]
fn record_event_in_dir_appends_and_updates_meta() {
    let tmp = tempfile::TempDir::new().unwrap();
    let rec_dir = tmp.path().join("recordings");
    fs::create_dir_all(&rec_dir).unwrap();

    let ev = SessionEvent {
        timestamp: 1000,
        event_type: "user".to_string(),
        tool: None,
        content: "fix the login bug".to_string(),
        working_dir: Some("/repo".to_string()),
        git_branch: Some("main".to_string()),
        exit_code: None,
    };
    record_event_in_dir(&rec_dir, "sess-1", "claude", &ev).unwrap();
    record_event_in_dir(&rec_dir, "sess-1", "claude", &ev).unwrap();

    let content = fs::read_to_string(rec_dir.join("sess-1.jsonl")).unwrap();
    assert_eq!(content.lines().count(), 2);

    let meta: SessionMeta =
        serde_json::from_str(&fs::read_to_string(rec_dir.join("sess-1.meta.json")).unwrap())
            .unwrap();
    assert_eq!(meta.user_turns, 2);
    assert_eq!(meta.source, "claude");
    assert_eq!(meta.title.as_deref(), Some("fix the login bug"));
}

#[test]
fn test_remove_recording_no_synced_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let rec_dir = tmp.path().join("recordings");
    fs::create_dir_all(&rec_dir).unwrap();

    let id = "test-no-sync";
    fs::write(rec_dir.join(format!("{}.jsonl", id)), "line1\n").unwrap();
    fs::write(rec_dir.join(format!("{}.meta.json", id)), "{}").unwrap();

    // Should succeed even without .synced file
    remove_recording_in_dir(&rec_dir, id).unwrap();

    assert!(!rec_dir.join(format!("{}.jsonl", id)).exists());
    assert!(!rec_dir.join(format!("{}.meta.json", id)).exists());
}
