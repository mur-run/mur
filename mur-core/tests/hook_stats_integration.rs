//! Integration: write real events, then compute + format through the full pipeline.

use mur_core::inject::event::{EventKind, NormalizedEvent};
use mur_core::inject::queue::{enqueue_to, read_all_events};
use mur_core::inject::stats::{compute, format_stats};

fn make_event(kind: EventKind, tool: Option<&str>, session: Option<&str>) -> NormalizedEvent {
    NormalizedEvent {
        kind,
        tool_provider: "claude".into(),
        query: None,
        tool_called: tool.map(str::to_owned),
        tool_input: None,
        stop_reason: None,
        session_id: session.map(str::to_owned),
        transcript_path: None,
        tool_response: None,
        cwd: None,
        duration_ms: None,
        is_duration_record: false,
    }
}

#[test]
fn stats_from_real_queue_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");

    let events_to_write = vec![
        make_event(EventKind::SessionStart, None, Some("sess_1")),
        make_event(EventKind::Prompt, None, Some("sess_1")),
        make_event(EventKind::Tool, Some("Edit"), Some("sess_1")),
        make_event(EventKind::Tool, Some("Bash"), Some("sess_1")),
        make_event(EventKind::Stop, None, Some("sess_1")),
        make_event(EventKind::Prompt, None, Some("sess_2")),
        make_event(EventKind::Tool, Some("Edit"), Some("sess_2")),
        make_event(EventKind::Stop, None, Some("sess_2")),
    ];
    for ev in &events_to_write {
        enqueue_to(ev, &path).unwrap();
    }

    let events = read_all_events(&path);
    assert_eq!(events.len(), 8);

    let stats = compute(&events);
    assert_eq!(stats.total, 8);
    assert_eq!(stats.unique_sessions, 2);
    assert_eq!(stats.by_kind["Prompt"], 2);
    assert_eq!(stats.by_kind["Tool"], 3);
    assert_eq!(stats.top_tools[0].0, "Edit");
    assert_eq!(stats.top_tools[0].1, 2);

    let out = format_stats(&stats, "/tmp/events.jsonl");
    assert!(out.contains("Total"), "output: {out}");
    assert!(out.contains("Edit"), "output: {out}");
    assert!(out.contains("Sessions:  2"), "output: {out}");
}

#[test]
fn stats_empty_queue_graceful() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no_events.jsonl");
    let events = read_all_events(&path);
    assert!(events.is_empty());
    let stats = compute(&events);
    let out = format_stats(&stats, &path.display().to_string());
    assert!(out.contains("No hook events"), "output: {out}");
}
