use std::collections::{HashMap, HashSet};

use crate::inject::event::{EventKind, NormalizedEvent};

#[allow(dead_code)] // called from cmd::hook_stats in Task 3
#[derive(Debug, Default)]
pub struct HookStats {
    pub total: usize,
    pub by_kind: HashMap<String, usize>,
    pub by_provider: HashMap<String, usize>,
    /// Top tools sorted descending by call count, truncated to 5.
    pub top_tools: Vec<(String, usize)>,
    pub unique_sessions: usize,
    pub latency_p50_ms: Option<u64>,
    pub latency_p95_ms: Option<u64>,
    pub latency_p99_ms: Option<u64>,
    /// The window these numbers actually describe. `None` when no record in
    /// the file carries a timestamp — which is every record written before
    /// #982, and is why this is reported rather than assumed.
    pub window_start: Option<chrono::DateTime<chrono::Utc>>,
    pub window_end: Option<chrono::DateTime<chrono::Utc>>,
    /// Records with no `recorded_at`. They still count toward every other
    /// number here; they simply cannot say when they happened.
    pub undated: usize,
}

#[allow(dead_code)] // called from cmd::hook_stats in Task 3
/// Compute over records, keeping the write-time window. `compute` is retained
/// for callers that have only events and therefore cannot describe a window.
pub fn compute_records(records: &[crate::inject::queue::QueueRecord]) -> HookStats {
    let events: Vec<NormalizedEvent> = records.iter().map(|r| r.event.clone()).collect();
    let mut stats = compute(&events);
    let dated: Vec<_> = records.iter().filter_map(|r| r.recorded_at).collect();
    stats.undated = records.len() - dated.len();
    stats.window_start = dated.iter().min().copied();
    stats.window_end = dated.iter().max().copied();
    stats
}

pub fn compute(events: &[NormalizedEvent]) -> HookStats {
    let mut by_kind: HashMap<String, usize> = HashMap::new();
    let mut by_provider: HashMap<String, usize> = HashMap::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut sessions: HashSet<String> = HashSet::new();

    for ev in events.iter().filter(|e| !e.is_duration_record) {
        let kind_key = match ev.kind {
            EventKind::Prompt => "Prompt",
            EventKind::Tool => "Tool",
            EventKind::Stop => "Stop",
            EventKind::SessionStart => "SessionStart",
        };
        *by_kind.entry(kind_key.to_owned()).or_default() += 1;
        *by_provider.entry(ev.tool_provider.clone()).or_default() += 1;
        if ev.kind == EventKind::Tool
            && let Some(tool) = &ev.tool_called
        {
            *tool_counts.entry(tool.clone()).or_default() += 1;
        }
        if let Some(sid) = &ev.session_id {
            sessions.insert(sid.clone());
        }
    }

    let mut top_tools: Vec<(String, usize)> = tool_counts.into_iter().collect();
    top_tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top_tools.truncate(5);

    let mut durations: Vec<u64> = events.iter().filter_map(|e| e.duration_ms).collect();
    durations.sort_unstable();

    fn percentile(sorted: &[u64], pct: f64) -> Option<u64> {
        if sorted.is_empty() {
            return None;
        }
        let idx = ((pct / 100.0) * (sorted.len() - 1) as f64).floor() as usize;
        Some(sorted[idx.min(sorted.len() - 1)])
    }

    HookStats {
        total: events.iter().filter(|e| !e.is_duration_record).count(),
        by_kind,
        by_provider,
        top_tools,
        unique_sessions: sessions.len(),
        latency_p50_ms: percentile(&durations, 50.0),
        latency_p95_ms: percentile(&durations, 95.0),
        latency_p99_ms: percentile(&durations, 99.0),
        // `compute` sees events only, so it cannot know the window. Filled in
        // by `compute_records`, which has the write-time metadata.
        window_start: None,
        window_end: None,
        undated: 0,
    }
}

#[allow(dead_code)] // called from cmd::hook_stats in Task 3
pub fn format_stats(stats: &HookStats, queue_path: &str) -> String {
    if stats.total == 0 {
        return format!("No hook events recorded yet.\nQueue: {queue_path}\n");
    }

    // Say what these numbers cover. Without this the totals are "since
    // whenever this file started", which the file cannot tell you — and after
    // rotation they would silently describe a different, also-unstated window.
    let window = match (stats.window_start, stats.window_end) {
        (Some(a), Some(b)) if stats.undated == 0 => {
            format!("Window: {} .. {}\n", a.to_rfc3339(), b.to_rfc3339())
        }
        (Some(a), Some(b)) => format!(
            "Window: {} .. {} (+{} undated record(s) written before timestamping)\n",
            a.to_rfc3339(),
            b.to_rfc3339(),
            stats.undated
        ),
        _ => format!(
            "Window: unknown — none of the {} record(s) carries a timestamp\n",
            stats.undated
        ),
    };

    // Compute column width from the widest label we'll print
    let kinds = ["Prompt", "Tool", "Stop", "SessionStart"];
    let max_kind_len = kinds.iter().map(|k| k.len()).max().unwrap_or(0);
    let max_tool_len = stats
        .top_tools
        .iter()
        .map(|(t, _)| t.len())
        .max()
        .unwrap_or(0);
    let max_prov_len = stats.by_provider.keys().map(|k| k.len()).max().unwrap_or(0);
    let col_w = (max_kind_len.max(max_tool_len).max(max_prov_len) + 2).max(16);

    let mut out = String::new();
    out.push_str(&format!("Hook events  ({})\n", queue_path));
    out.push_str(&format!("  {window}"));
    out.push_str(&format!("  Total:     {}\n", stats.total));
    out.push_str(&format!("  Sessions:  {}\n", stats.unique_sessions));

    out.push_str("\nBy kind:\n");
    for kind in kinds {
        if let Some(n) = stats.by_kind.get(kind) {
            out.push_str(&format!("  {:<col_w$}{}\n", format!("{kind}:"), n));
        }
    }

    if !stats.top_tools.is_empty() {
        out.push_str("\nTop tools:\n");
        for (tool, n) in &stats.top_tools {
            out.push_str(&format!("  {:<col_w$}{}\n", format!("{tool}:"), n));
        }
    }

    if stats.by_provider.len() > 1 {
        out.push_str("\nProviders:\n");
        let mut providers: Vec<_> = stats.by_provider.iter().collect();
        providers.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (prov, n) in providers {
            out.push_str(&format!("  {:<col_w$}{}\n", format!("{prov}:"), n));
        }
    }

    if stats.latency_p50_ms.is_some()
        || stats.latency_p95_ms.is_some()
        || stats.latency_p99_ms.is_some()
    {
        out.push_str("\nLatency (prompt+tool hooks with timing):\n");
        if let Some(p50) = stats.latency_p50_ms {
            out.push_str(&format!("  p50: {p50} ms\n"));
        }
        if let Some(p95) = stats.latency_p95_ms {
            out.push_str(&format!("  p95: {p95} ms\n"));
        }
        if let Some(p99) = stats.latency_p99_ms {
            out.push_str(&format!("  p99: {p99} ms\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #979: the totals used to be "since whenever this file started", which
    /// the file could not tell you — and after rotation they would describe a
    /// different, equally unstated window. The report has to say what it
    /// covers, including when it cannot.
    #[test]
    fn the_report_states_the_window_it_covers() {
        use crate::inject::queue::QueueRecord;
        let t0 = chrono::Utc::now() - chrono::Duration::hours(3);
        let t1 = chrono::Utc::now();
        let recs = vec![
            QueueRecord {
                recorded_at: Some(t0),
                event: prompt_ev("a"),
            },
            QueueRecord {
                recorded_at: Some(t1),
                event: prompt_ev("b"),
            },
        ];
        let out = format_stats(&compute_records(&recs), "/tmp/q.jsonl");
        assert!(out.contains("Window:"), "{out}");
        assert!(out.contains(&t0.to_rfc3339()), "start missing:\n{out}");
        assert!(!out.contains("undated"), "nothing is undated here:\n{out}");
    }

    /// Records written before stamping existed carry no time. They must be
    /// counted and named, not quietly folded into a window they are not in.
    #[test]
    fn undated_records_are_named_not_hidden() {
        use crate::inject::queue::QueueRecord;
        let recs = vec![
            QueueRecord {
                recorded_at: Some(chrono::Utc::now()),
                event: prompt_ev("a"),
            },
            QueueRecord {
                recorded_at: None,
                event: prompt_ev("b"),
            },
        ];
        let out = format_stats(&compute_records(&recs), "/tmp/q.jsonl");
        assert!(out.contains("1 undated record"), "{out}");
    }

    /// And when NOTHING is dated — the shape of every queue that predates
    /// #982 — the report says the window is unknown rather than printing a
    /// number that looks like a measurement.
    #[test]
    fn a_wholly_undated_queue_reports_an_unknown_window() {
        use crate::inject::queue::QueueRecord;
        let recs = vec![
            QueueRecord {
                recorded_at: None,
                event: prompt_ev("a"),
            },
            QueueRecord {
                recorded_at: None,
                event: prompt_ev("b"),
            },
        ];
        let out = format_stats(&compute_records(&recs), "/tmp/q.jsonl");
        assert!(out.contains("Window: unknown"), "{out}");
        assert!(out.contains("2 record(s)"), "{out}");
    }

    fn prompt_ev(q: &str) -> NormalizedEvent {
        NormalizedEvent {
            kind: EventKind::Prompt,
            tool_provider: "claude".into(),
            query: Some(q.into()),
            tool_called: None,
            tool_input: None,
            stop_reason: None,
            session_id: Some("s".into()),
            transcript_path: None,
            tool_response: None,
            cwd: None,
            duration_ms: None,
            is_duration_record: false,
        }
    }
    use crate::inject::event::{EventKind, NormalizedEvent};

    fn ev(
        kind: EventKind,
        provider: &str,
        tool: Option<&str>,
        session: Option<&str>,
    ) -> NormalizedEvent {
        NormalizedEvent {
            kind,
            tool_provider: provider.into(),
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
    fn compute_empty_returns_all_zeroes() {
        let stats = compute(&[]);
        assert_eq!(stats.total, 0);
        assert_eq!(stats.unique_sessions, 0);
        assert!(stats.by_kind.is_empty());
        assert!(stats.top_tools.is_empty());
    }

    #[test]
    fn compute_counts_events_by_kind() {
        let events = vec![
            ev(EventKind::Prompt, "claude", None, None),
            ev(EventKind::Prompt, "claude", None, None),
            ev(EventKind::Tool, "claude", Some("Edit"), None),
            ev(EventKind::Stop, "claude", None, None),
        ];
        let stats = compute(&events);
        assert_eq!(stats.total, 4);
        assert_eq!(stats.by_kind["Prompt"], 2);
        assert_eq!(stats.by_kind["Tool"], 1);
        assert_eq!(stats.by_kind["Stop"], 1);
    }

    #[test]
    fn compute_top_tools_sorted_descending() {
        let events = vec![
            ev(EventKind::Tool, "claude", Some("Bash"), None),
            ev(EventKind::Tool, "claude", Some("Bash"), None),
            ev(EventKind::Tool, "claude", Some("Bash"), None),
            ev(EventKind::Tool, "claude", Some("Edit"), None),
            ev(EventKind::Tool, "claude", Some("Edit"), None),
            ev(EventKind::Tool, "claude", Some("Write"), None),
        ];
        let stats = compute(&events);
        assert_eq!(stats.top_tools[0], ("Bash".to_owned(), 3));
        assert_eq!(stats.top_tools[1], ("Edit".to_owned(), 2));
        assert_eq!(stats.top_tools[2], ("Write".to_owned(), 1));
    }

    #[test]
    fn compute_top_tools_truncated_to_5() {
        let tools = ["A", "B", "C", "D", "E", "F", "G"];
        let events: Vec<NormalizedEvent> = tools
            .iter()
            .map(|t| ev(EventKind::Tool, "claude", Some(t), None))
            .collect();
        let stats = compute(&events);
        assert!(stats.top_tools.len() <= 5);
    }

    #[test]
    fn compute_unique_sessions() {
        let events = vec![
            ev(EventKind::Prompt, "claude", None, Some("sess_a")),
            ev(EventKind::Prompt, "claude", None, Some("sess_a")),
            ev(EventKind::Prompt, "claude", None, Some("sess_b")),
            ev(EventKind::Prompt, "claude", None, None),
        ];
        let stats = compute(&events);
        assert_eq!(stats.unique_sessions, 2);
    }

    #[test]
    fn compute_provider_breakdown() {
        let events = vec![
            ev(EventKind::Prompt, "claude", None, None),
            ev(EventKind::Prompt, "claude", None, None),
            ev(EventKind::Prompt, "gemini", None, None),
        ];
        let stats = compute(&events);
        assert_eq!(stats.by_provider["claude"], 2);
        assert_eq!(stats.by_provider["gemini"], 1);
    }

    #[test]
    fn format_stats_empty_shows_no_events() {
        let stats = HookStats::default();
        let out = format_stats(&stats, "~/.mur/queue/events.jsonl");
        assert!(out.contains("No hook events"), "got: {out}");
    }

    #[test]
    fn format_stats_non_empty_contains_key_fields() {
        let events = vec![
            ev(EventKind::Prompt, "claude", None, Some("s1")),
            ev(EventKind::Tool, "claude", Some("Edit"), Some("s1")),
        ];
        let stats = compute(&events);
        let out = format_stats(&stats, "/tmp/events.jsonl");
        assert!(out.contains("Total"), "got: {out}");
        assert!(out.contains("Prompt"), "got: {out}");
        assert!(out.contains("Edit"), "got: {out}");
    }

    #[test]
    fn compute_tool_called_on_non_tool_event_not_counted() {
        // tool_called on a Prompt event must NOT appear in top_tools
        let events = vec![NormalizedEvent {
            kind: EventKind::Prompt,
            tool_provider: "claude".into(),
            query: None,
            tool_called: Some("Edit".to_owned()), // would be counted without the kind guard
            tool_input: None,
            stop_reason: None,
            session_id: None,
            transcript_path: None,
            tool_response: None,
            cwd: None,
            duration_ms: None,
            is_duration_record: false,
        }];
        let stats = compute(&events);
        assert!(
            stats.top_tools.is_empty(),
            "tool_called on Prompt must not be counted"
        );
    }

    #[test]
    fn compute_top_tools_exactly_5_when_more_available() {
        let tools = ["A", "B", "C", "D", "E", "F", "G"];
        let events: Vec<NormalizedEvent> = tools
            .iter()
            .map(|t| ev(EventKind::Tool, "claude", Some(t), None))
            .collect();
        let stats = compute(&events);
        assert_eq!(stats.top_tools.len(), 5, "must be exactly 5, not just ≤ 5");
    }

    #[test]
    fn format_stats_single_provider_omits_providers_section() {
        let events = vec![ev(EventKind::Prompt, "claude", None, None)];
        let stats = compute(&events);
        let out = format_stats(&stats, "/tmp/e.jsonl");
        assert!(
            !out.contains("Providers"),
            "single provider must not show Providers section; got: {out}"
        );
    }

    #[test]
    fn format_stats_includes_queue_path_and_sessions() {
        let events = vec![
            ev(EventKind::Prompt, "claude", None, Some("s1")),
            ev(EventKind::Stop, "claude", None, Some("s1")),
        ];
        let stats = compute(&events);
        let out = format_stats(&stats, "/custom/path/events.jsonl");
        assert!(
            out.contains("/custom/path/events.jsonl"),
            "queue path must appear; got: {out}"
        );
        assert!(
            out.contains("Sessions:  1"),
            "session count must appear; got: {out}"
        );
    }

    #[test]
    fn latency_percentiles_from_events() {
        let events: Vec<NormalizedEvent> = (1u64..=100)
            .map(|i| NormalizedEvent {
                kind: EventKind::Prompt,
                tool_provider: "claude".into(),
                query: None,
                tool_called: None,
                tool_input: None,
                stop_reason: None,
                session_id: None,
                transcript_path: None,
                tool_response: None,
                cwd: None,
                duration_ms: Some(i),
                is_duration_record: false,
            })
            .collect();
        let stats = compute(&events);
        assert_eq!(stats.latency_p50_ms, Some(50));
        assert_eq!(stats.latency_p95_ms, Some(95));
        assert_eq!(stats.latency_p99_ms, Some(99));
    }

    #[test]
    fn latency_none_when_no_durations() {
        let events = vec![NormalizedEvent {
            kind: EventKind::Prompt,
            tool_provider: "claude".into(),
            query: None,
            tool_called: None,
            tool_input: None,
            stop_reason: None,
            session_id: None,
            transcript_path: None,
            tool_response: None,
            cwd: None,
            duration_ms: None,
            is_duration_record: false,
        }];
        let stats = compute(&events);
        assert!(stats.latency_p50_ms.is_none());
        assert!(stats.latency_p99_ms.is_none());
    }

    #[test]
    fn duration_records_excluded_from_counts() {
        let events = vec![
            NormalizedEvent {
                kind: EventKind::Prompt,
                tool_provider: "claude".into(),
                query: None,
                tool_called: None,
                tool_input: None,
                stop_reason: None,
                session_id: None,
                transcript_path: None,
                tool_response: None,
                cwd: None,
                duration_ms: Some(42),
                is_duration_record: true,
            },
            NormalizedEvent {
                kind: EventKind::Prompt,
                tool_provider: "claude".into(),
                query: None,
                tool_called: None,
                tool_input: None,
                stop_reason: None,
                session_id: None,
                transcript_path: None,
                tool_response: None,
                cwd: None,
                duration_ms: None,
                is_duration_record: false,
            },
        ];
        let stats = compute(&events);
        assert_eq!(stats.total, 1, "duration records must not be counted");
        assert_eq!(
            stats.latency_p50_ms,
            Some(42),
            "duration from timing record must appear"
        );
    }
}
