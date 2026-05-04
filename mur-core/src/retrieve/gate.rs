//! Adaptive query gate — decides whether to trigger pattern retrieval.

use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// 0 tokens — pure ack / greeting / noise
    Skip,
    /// Capability index only (~150-300 tokens, SessionStart layer)
    L0,
    /// 1-3 pattern snippets (~500 tokens)
    L1,
    /// Full body + linked workflows (~1500-2000 tokens)
    L2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateOutcome {
    pub tier: Tier,
    pub score: f32,
    pub reasons: Vec<&'static str>,
}

// ─── Intent score ────────────────────────────────────────────────────────────

static ACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(ok|okay|好|好的|thanks|thank you|thx|sure|yes|no|nope|對|不對|是|嗯|沒問題|了解|收到|got it|understood|符合|fine|cool|nice|great)[\s\.!\?]*$").unwrap()
});

static META_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/(help|status|model|clear|usage|exit|quit|effort|fast|review|init|config|mcp|cost|memory|hooks?|permissions?|agents?|todos?)\b").unwrap()
});

static QUESTION_EN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(why|what is|what's|how does|explain)\b").unwrap()
});

static QUESTION_ZH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(為什麼|是什麼|解釋一下|怎麼回事)").unwrap()
});

static CODE_IDENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\w+\.[a-zA-Z]{1,5}\b|\bfn\s+\w+|\w+::\w+|\b[a-z]+_[a-z_]+\b)").unwrap()
});

static ACTION_VERB_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(實作|實現|修|改|加|建立|刪除|測試|refactor|implement|build|fix|add|remove|delete|create|test|debug|deploy|migrate|rewrite|integrate|wire|hook|extract)\b").unwrap()
});

const TECH_TERMS: &[&str] = &[
    "tokio", "async", "await", "spawn", "select", "trait", "struct", "enum", "impl",
    "test", "build", "debug", "deploy", "lint", "format", "refactor", "migrate",
    "error", "panic", "result", "option", "vec", "hashmap", "btreemap",
    "api", "endpoint", "request", "response", "header", "json", "yaml",
    "database", "schema", "table", "column", "index", "query", "transaction",
    "worker", "queue", "daemon", "thread", "lock", "channel", "future",
    "tcp", "http", "https", "ssl", "tls", "noise", "websocket",
    "docker", "kubernetes", "ci", "cd", "pipeline", "hook", "skill", "agent",
    "vector", "embedding", "retrieval", "rag", "llm", "prompt", "pattern",
];

fn count_tech_terms(query_lower: &str) -> usize {
    TECH_TERMS.iter().filter(|t| query_lower.contains(*t)).count()
}

pub(crate) fn intent_score(query: &str) -> f32 {
    let trimmed = query.trim();
    if ACK_RE.is_match(trimmed) {
        return 0.0;
    }
    if META_RE.is_match(trimmed) {
        return 0.0;
    }
    if QUESTION_EN_RE.is_match(trimmed) || QUESTION_ZH_RE.is_match(trimmed) {
        return 0.3;
    }

    let lower = trimmed.to_lowercase();
    let tech_count = count_tech_terms(&lower);
    let char_count = trimmed.chars().count();

    if char_count > 80 && tech_count >= 2 {
        return 0.9;
    }
    if ACTION_VERB_RE.is_match(trimmed) {
        return 0.8;
    }
    if CODE_IDENT_RE.is_match(trimmed) {
        return 0.7;
    }
    0.5
}

// ─── Tool signal score ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ToolSignalInput {
    pub tool: String,
    pub bash_command: Option<String>,
}

const BUILD_TEST_RUNNERS: &[&str] = &[
    "cargo", "npm", "yarn", "pnpm", "bun", "pytest", "go", "make", "docker",
    "rustc", "gcc", "clang", "mvn", "gradle", "swift", "xcodebuild",
];

pub(crate) fn tool_signal_score(history: &[ToolSignalInput]) -> f32 {
    if history.is_empty() {
        return 0.1;
    }

    let mut has_edit = false;
    let mut has_build = false;
    let mut has_mcp = false;
    let mut only_read = true;

    for entry in history {
        match entry.tool.as_str() {
            "Edit" | "Write" | "NotebookEdit" => {
                has_edit = true;
                only_read = false;
            }
            "Read" | "Glob" | "Grep" => {}
            "Bash" => {
                only_read = false;
                if let Some(cmd) = &entry.bash_command {
                    let first_word = cmd.split_whitespace().next().unwrap_or("");
                    if BUILD_TEST_RUNNERS.iter().any(|r| first_word == *r) {
                        has_build = true;
                    }
                }
            }
            t if t.starts_with("mcp__") => {
                has_mcp = true;
                only_read = false;
            }
            _ => {
                only_read = false;
            }
        }
    }

    if has_edit { return 0.9; }
    if has_build { return 0.8; }
    if has_mcp { return 0.7; }
    if only_read { return 0.4; }
    0.5
}

// ─── Session state score ──────────────────────────────────────────────────────

use chrono::Duration;

#[derive(Debug, Clone)]
pub struct SessionStateInput {
    pub age: Duration,
    pub seconds_since_last_edit: Option<i64>,
}

pub(crate) fn session_state_score(input: &SessionStateInput) -> f32 {
    if let Some(s) = input.seconds_since_last_edit {
        if s < 60 {
            return 0.9;
        }
    }
    if input.age < Duration::seconds(30) {
        return 0.7;
    }
    if input.age > Duration::minutes(30) && input.seconds_since_last_edit.is_none() {
        return 0.3;
    }
    0.5
}

// ─── Query quality score ──────────────────────────────────────────────────────

use crate::capture::noise_filter::{filter, FilterResult};

pub(crate) fn query_quality_score(query: &str) -> f32 {
    match filter(query) {
        FilterResult::Pass => 1.0,
        FilterResult::Noise(_) => 0.0,
    }
}

// ─── Session recording reader ─────────────────────────────────────────────────

use std::path::Path;

/// Read the last `n` tool calls from the active session recording.
///
/// Returns oldest-first. Returns empty Vec on any error — gate degrades
/// gracefully to "no history" signal.
pub(crate) fn read_recent_tool_history(mur_dir: &Path, n: usize) -> Vec<ToolSignalInput> {
    let active_path = mur_dir.join("session/active.json");
    let Ok(active_raw) = std::fs::read_to_string(&active_path) else {
        return Vec::new();
    };
    let Ok(active_json): Result<serde_json::Value, _> = serde_json::from_str(&active_raw) else {
        return Vec::new();
    };
    let Some(session_id) = active_json.get("session_id").and_then(|v| v.as_str()) else {
        return Vec::new();
    };

    let recording = mur_dir.join("session/recordings").join(format!("{session_id}.jsonl"));
    let Ok(content) = std::fs::read_to_string(&recording) else {
        return Vec::new();
    };

    let mut tool_events: Vec<ToolSignalInput> = content
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            if v.get("event_type")?.as_str()? != "tool_call" {
                return None;
            }
            let tool = v.get("tool")?.as_str()?.to_string();
            let bash_command = v
                .get("content")
                .and_then(|c| c.as_str())
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|cv| cv.get("command")?.as_str().map(String::from));
            Some(ToolSignalInput { tool, bash_command })
        })
        .collect();

    let total = tool_events.len();
    if total > n {
        tool_events.drain(..total - n);
    }
    tool_events
}

/// Evaluate whether a query should trigger pattern retrieval.
/// Stub implementation — replaced in Task 7 with composite scoring.
pub fn evaluate_query(query: &str) -> GateOutcome {
    let _ = query;
    GateOutcome { tier: Tier::L1, score: 0.5, reasons: vec![] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_ordering() {
        assert!(Tier::Skip < Tier::L0);
        assert!(Tier::L0 < Tier::L1);
        assert!(Tier::L1 < Tier::L2);
    }

    #[test]
    fn test_outcome_construction() {
        let o = GateOutcome { tier: Tier::L1, score: 0.62, reasons: vec!["intent: action verb"] };
        assert_eq!(o.tier, Tier::L1);
        assert!((o.score - 0.62).abs() < 1e-6);
        assert_eq!(o.reasons.len(), 1);
    }

    #[test]
    fn intent_pure_ack_zero() {
        assert_eq!(intent_score("ok"), 0.0);
        assert_eq!(intent_score("好"), 0.0);
        assert_eq!(intent_score("thanks"), 0.0);
        assert_eq!(intent_score("符合"), 0.0);
        assert_eq!(intent_score("OK!"), 0.0);
    }

    #[test]
    fn intent_meta_command_zero() {
        assert_eq!(intent_score("/help"), 0.0);
        assert_eq!(intent_score("/status"), 0.0);
        assert_eq!(intent_score("/model gpt-4"), 0.0);
    }

    #[test]
    fn intent_question_low() {
        assert!((intent_score("為什麼會這樣") - 0.3).abs() < 1e-6);
        assert!((intent_score("what is RAG") - 0.3).abs() < 1e-6);
    }

    #[test]
    fn intent_code_identifier_mid() {
        assert!((intent_score("look at mod.rs") - 0.7).abs() < 1e-6);
        assert!((intent_score("the fn handle_event() is broken") - 0.7).abs() < 1e-6);
    }

    #[test]
    fn intent_action_verb_high() {
        assert!((intent_score("實作 adaptive gate") - 0.8).abs() < 1e-6);
        assert!((intent_score("refactor the auth module") - 0.8).abs() < 1e-6);
        assert!((intent_score("fix the build error") - 0.8).abs() < 1e-6);
    }

    #[test]
    fn intent_long_technical_max() {
        let q = "I want to add a new tokio worker that subscribes to events.jsonl and runs LLM extraction in the background pool";
        assert!((intent_score(q) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn intent_fallback_mid() {
        assert!((intent_score("can you help me with this thing") - 0.5).abs() < 1e-6);
    }

    fn ts(tool: &str, cmd: Option<&str>) -> ToolSignalInput {
        ToolSignalInput { tool: tool.into(), bash_command: cmd.map(String::from) }
    }

    #[test]
    fn tool_signal_no_history_low() {
        assert!((tool_signal_score(&[]) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_only_read_only() {
        let h = vec![ts("Read", None), ts("Glob", None), ts("Grep", None)];
        assert!((tool_signal_score(&h) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_edit_present_high() {
        let h = vec![ts("Read", None), ts("Edit", None)];
        assert!((tool_signal_score(&h) - 0.9).abs() < 1e-6);
        let h2 = vec![ts("Write", None)];
        assert!((tool_signal_score(&h2) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_build_command_high() {
        let h = vec![ts("Bash", Some("cargo test --workspace"))];
        assert!((tool_signal_score(&h) - 0.8).abs() < 1e-6);
        let h2 = vec![ts("Bash", Some("npm run build"))];
        assert!((tool_signal_score(&h2) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_mcp_mid() {
        let h = vec![ts("mcp__chrome-devtools__navigate_page", None)];
        assert!((tool_signal_score(&h) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_edit_wins_over_read() {
        let h = vec![ts("Read", None), ts("Bash", Some("ls")), ts("Edit", None)];
        assert!((tool_signal_score(&h) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn session_fresh_high() {
        let s = SessionStateInput { age: Duration::seconds(10), seconds_since_last_edit: None };
        assert!((session_state_score(&s) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn session_active_edit_max() {
        let s = SessionStateInput { age: Duration::minutes(5), seconds_since_last_edit: Some(30) };
        assert!((session_state_score(&s) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn session_idle_low() {
        let s = SessionStateInput { age: Duration::minutes(45), seconds_since_last_edit: None };
        assert!((session_state_score(&s) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn session_default_mid() {
        let s = SessionStateInput { age: Duration::minutes(5), seconds_since_last_edit: Some(600) };
        assert!((session_state_score(&s) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn quality_pass_full() {
        assert!((query_quality_score("Refactor the gate module to support tier scoring") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn quality_noise_zero() {
        assert!((query_quality_score("ok") - 0.0).abs() < 1e-6);
        assert!((query_quality_score("👍") - 0.0).abs() < 1e-6);
        assert!((query_quality_score("") - 0.0).abs() < 1e-6);
    }

    #[test]
    fn read_history_from_recordings() {
        use std::io::Write as _;
        let tmp = tempfile::TempDir::new().unwrap();
        let session_dir = tmp.path().join("session/recordings");
        std::fs::create_dir_all(&session_dir).unwrap();

        let active = serde_json::json!({"session_id": "test-sess"});
        std::fs::write(tmp.path().join("session/active.json"),
            serde_json::to_string(&active).unwrap()).unwrap();

        let mut f = std::fs::File::create(session_dir.join("test-sess.jsonl")).unwrap();
        writeln!(f, r#"{{"event_type":"tool_call","tool":"Read","content":"{{}}"}}"#).unwrap();
        writeln!(f, r#"{{"event_type":"tool_call","tool":"Bash","content":"{{\"command\":\"cargo test\"}}"}}"#).unwrap();
        writeln!(f, r#"{{"event_type":"tool_call","tool":"Edit","content":"{{}}"}}"#).unwrap();

        let h = read_recent_tool_history(tmp.path(), 5);
        assert_eq!(h.len(), 3);
        assert_eq!(h[2].tool, "Edit");
        assert_eq!(h[1].tool, "Bash");
        assert_eq!(h[1].bash_command.as_deref(), Some("cargo test"));
    }

    #[test]
    fn read_history_missing_active_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let h = read_recent_tool_history(tmp.path(), 5);
        assert!(h.is_empty());
    }
}
