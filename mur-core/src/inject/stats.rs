use std::collections::HashMap;

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
}

#[allow(dead_code)] // called from cmd::hook_stats in Task 3
pub fn compute(events: &[NormalizedEvent]) -> HookStats {
    let mut by_kind: HashMap<String, usize> = HashMap::new();
    let mut by_provider: HashMap<String, usize> = HashMap::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut sessions: std::collections::HashSet<String> = std::collections::HashSet::new();

    for ev in events {
        let kind_key = match ev.kind {
            EventKind::Prompt => "Prompt",
            EventKind::Tool => "Tool",
            EventKind::Stop => "Stop",
            EventKind::SessionStart => "SessionStart",
        };
        *by_kind.entry(kind_key.to_owned()).or_default() += 1;
        *by_provider.entry(ev.tool_provider.clone()).or_default() += 1;
        if let Some(tool) = &ev.tool_called {
            *tool_counts.entry(tool.clone()).or_default() += 1;
        }
        if let Some(sid) = &ev.session_id {
            sessions.insert(sid.clone());
        }
    }

    let mut top_tools: Vec<(String, usize)> = tool_counts.into_iter().collect();
    top_tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top_tools.truncate(5);

    HookStats {
        total: events.len(),
        by_kind,
        by_provider,
        top_tools,
        unique_sessions: sessions.len(),
    }
}

#[allow(dead_code)] // called from cmd::hook_stats in Task 3
pub fn format_stats(stats: &HookStats, queue_path: &str) -> String {
    if stats.total == 0 {
        return format!("No hook events recorded yet.\nQueue: {queue_path}\n");
    }

    let mut out = String::new();
    out.push_str(&format!("Hook events  ({})\n", queue_path));
    out.push_str(&format!("  Total:     {}\n", stats.total));
    out.push_str(&format!("  Sessions:  {}\n", stats.unique_sessions));

    out.push_str("\nBy kind:\n");
    for kind in ["Prompt", "Tool", "Stop", "SessionStart"] {
        if let Some(n) = stats.by_kind.get(kind) {
            out.push_str(&format!("  {:<16}{}\n", format!("{kind}:"), n));
        }
    }

    if !stats.top_tools.is_empty() {
        out.push_str("\nTop tools:\n");
        for (tool, n) in &stats.top_tools {
            out.push_str(&format!("  {:<16}{}\n", format!("{tool}:"), n));
        }
    }

    if stats.by_provider.len() > 1 {
        out.push_str("\nProviders:\n");
        let mut providers: Vec<_> = stats.by_provider.iter().collect();
        providers.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (prov, n) in providers {
            out.push_str(&format!("  {:<16}{}\n", format!("{prov}:"), n));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
