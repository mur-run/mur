//! Post-session feedback analyzer.
//!
//! After an AI CLI session ends, analyze whether injected patterns were
//! helpful, contradicted, or ignored — pure keyword/string matching, no LLM.

use serde::{Deserialize, Serialize};

/// Record of a pattern that was injected into a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectedPatternRecord {
    /// Pattern name (kebab-case)
    pub name: String,
    /// First 100 chars of what was injected
    pub snippet: String,
}

/// What happened to an injected pattern during the session.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalType {
    /// AI used the pattern and user didn't object
    Reinforced,
    /// User explicitly contradicted or corrected the pattern
    Contradicted,
    /// Pattern was injected but never referenced
    Ignored,
}

/// Feedback signal for a single pattern after session analysis.
#[derive(Debug, Clone)]
pub struct SessionFeedback {
    /// Pattern name
    pub pattern_name: String,
    /// What happened
    pub signal: SignalType,
    /// The transcript line that triggered this signal (if any)
    pub evidence: Option<String>,
    /// How much to adjust confidence
    pub confidence_delta: f64,
}

/// Injection record written to ~/.mur/last_injection.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectionRecord {
    pub timestamp: String,
    pub query: String,
    pub project: String,
    pub patterns: Vec<InjectedPatternRecord>,
}

// ─── Contradiction phrases ────────────────────────────────────────

const CONTRADICTION_EN: &[&str] = &[
    "don't use",
    "do not use",
    "instead of",
    "wrong",
    "actually,",
    "actually ",
    "no,",
    "no ",
    "stop using",
    "incorrect",
    "shouldn't use",
    "should not use",
    "not recommended",
    "deprecated",
    "avoid",
];

const CONTRADICTION_ZH: &[&str] = &[
    "不要用",
    "別用",
    "改用",
    "錯了",
    "不對",
    "應該用",
    "不建議",
    "不推薦",
];

/// Analyze a session transcript against injected patterns.
///
/// Returns one `SessionFeedback` per injected pattern.
/// Designed to complete in <10ms for typical transcripts.
pub fn analyze_session_feedback(
    transcript: &str,
    injected: &[InjectedPatternRecord],
) -> Vec<SessionFeedback> {
    if injected.is_empty() {
        return Vec::new();
    }

    let transcript_lower = transcript.to_lowercase();
    let transcript_lines: Vec<&str> = transcript.lines().collect();

    injected
        .iter()
        .map(|record| analyze_single_pattern(record, &transcript_lower, &transcript_lines))
        .collect()
}

fn analyze_single_pattern(
    record: &InjectedPatternRecord,
    transcript_lower: &str,
    transcript_lines: &[&str],
) -> SessionFeedback {
    // Extract keywords from the pattern snippet (words ≥ 3 chars, lowered)
    let pattern_keywords = extract_keywords(&record.snippet);
    let name_keywords = extract_keywords(&record.name);

    // All keywords to match against
    let all_keywords: Vec<&str> = pattern_keywords
        .iter()
        .chain(name_keywords.iter())
        .map(|s| s.as_str())
        .collect();

    // 1) Check for contradiction: negative phrase near pattern keywords
    if let Some(evidence) = find_contradiction(transcript_lines, &all_keywords, &record.snippet) {
        return SessionFeedback {
            pattern_name: record.name.clone(),
            signal: SignalType::Contradicted,
            evidence: Some(evidence),
            confidence_delta: -0.10,
        };
    }

    // 2) Check for reinforcement: pattern keywords appear in transcript
    if has_keyword_overlap(transcript_lower, &all_keywords) {
        return SessionFeedback {
            pattern_name: record.name.clone(),
            signal: SignalType::Reinforced,
            evidence: None,
            confidence_delta: 0.03,
        };
    }

    // 3) Default: pattern was ignored
    SessionFeedback {
        pattern_name: record.name.clone(),
        signal: SignalType::Ignored,
        evidence: None,
        confidence_delta: -0.01,
    }
}

/// Common English stop-words that are too generic for keyword matching.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "use", "with", "this", "that", "from", "are", "was",
    "were", "been", "being", "have", "has", "had", "does", "did", "will",
    "would", "could", "should", "may", "might", "can", "shall", "not", "but",
    "all", "any", "each", "every", "both", "few", "more", "most", "other",
    "some", "such", "only", "than", "too", "very", "just", "into", "also",
    "how", "when", "where", "which", "while", "who", "whom", "what", "why",
    "new", "old",
];

/// Extract meaningful keywords from text (lowered, deduped, no stop words).
fn extract_keywords(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    // Split on anything that isn't alphanumeric or CJK
    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric() && !is_cjk(c))
        .filter(|w| {
            // CJK chars pass through
            if w.chars().any(is_cjk) {
                return true;
            }
            // English: require ≥ 3 chars and not a stop word
            w.len() >= 3 && !STOP_WORDS.contains(w)
        })
        .map(String::from)
        .collect();

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    words
        .into_iter()
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}')
}

/// Scan transcript lines for contradiction phrases near pattern keywords.
///
/// A "contradiction" is: a line contains BOTH a negative phrase AND
/// at least one keyword from the pattern snippet.
fn find_contradiction(
    lines: &[&str],
    keywords: &[&str],
    snippet: &str,
) -> Option<String> {
    let snippet_lower = snippet.to_lowercase();
    let snippet_words: Vec<&str> = snippet_lower
        .split(|c: char| !c.is_alphanumeric() && !is_cjk(c))
        .filter(|w| {
            if w.chars().any(is_cjk) {
                return true;
            }
            w.len() >= 3 && !STOP_WORDS.contains(w)
        })
        .collect();

    for line in lines {
        let lower_line = line.to_lowercase();

        // Check if this line contains a contradiction phrase
        let has_en = CONTRADICTION_EN.iter().any(|p| lower_line.contains(p));
        let has_zh = CONTRADICTION_ZH.iter().any(|p| lower_line.contains(p));

        if !has_en && !has_zh {
            continue;
        }

        // Check if a pattern keyword appears near the contradiction phrase
        // (within ~10 words). This proximity check reduces false positives
        // where a generic keyword like "error" appears far from "instead of".
        let mut all_terms: Vec<&str> = keywords.to_vec();
        all_terms.extend(snippet_words.iter().copied());
        all_terms.sort_unstable();
        all_terms.dedup();

        let all_phrases: Vec<&str> = if has_en {
            CONTRADICTION_EN.iter().filter(|p| lower_line.contains(**p)).copied().collect()
        } else {
            CONTRADICTION_ZH.iter().filter(|p| lower_line.contains(**p)).copied().collect()
        };

        let has_nearby_keyword = all_phrases.iter().any(|phrase| {
            all_terms.iter().any(|term| {
                is_near(&lower_line, phrase, term, 10)
            })
        });

        if has_nearby_keyword {
            // Truncate evidence to 200 chars
            let evidence = if line.len() > 200 {
                format!("{}...", &line[..200])
            } else {
                line.to_string()
            };
            return Some(evidence);
        }
    }

    None
}

/// Check if two substrings appear within `max_words` of each other in text.
fn is_near(text: &str, phrase_a: &str, phrase_b: &str, max_words: usize) -> bool {
    // Find positions of both phrases
    let pos_a = match text.find(phrase_a) {
        Some(p) => p,
        None => return false,
    };
    let pos_b = match text.find(phrase_b) {
        Some(p) => p,
        None => return false,
    };

    // Count words between the end of the earlier phrase and start of the later
    let (earlier_end, later_start) = if pos_a <= pos_b {
        (pos_a + phrase_a.len(), pos_b)
    } else {
        (pos_b + phrase_b.len(), pos_a)
    };

    if later_start <= earlier_end {
        // Overlapping or adjacent
        return true;
    }

    let between = &text[earlier_end..later_start];
    let word_count = between.split_whitespace().count();
    word_count <= max_words
}

/// Check if the transcript contains meaningful overlap with pattern keywords.
///
/// Requires at least 2 keyword matches (or 1 if pattern has few keywords).
fn has_keyword_overlap(transcript_lower: &str, keywords: &[&str]) -> bool {
    if keywords.is_empty() {
        return false;
    }

    let matches: usize = keywords
        .iter()
        .filter(|kw| transcript_lower.contains(**kw))
        .count();

    // Require at least 2 matches, or 1 if pattern has ≤ 2 keywords
    let threshold = if keywords.len() <= 2 { 1 } else { 2 };
    matches >= threshold
}

/// Write the injection tracking record to ~/.mur/last_injection.json
pub fn write_injection_record(record: &InjectionRecord) -> std::io::Result<()> {
    let path = injection_record_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&path, json)
}

/// Read the injection tracking record from ~/.mur/last_injection.json
pub fn read_injection_record() -> anyhow::Result<InjectionRecord> {
    let path = injection_record_path();
    let data = std::fs::read_to_string(&path)?;
    let record: InjectionRecord = serde_json::from_str(&data)?;
    Ok(record)
}

fn injection_record_path() -> std::path::PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("last_injection.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, snippet: &str) -> InjectedPatternRecord {
        InjectedPatternRecord {
            name: name.into(),
            snippet: snippet.into(),
        }
    }

    // ─── Contradiction detection: English ─────────────────────────

    #[test]
    fn test_contradiction_dont_use() {
        let transcript = "User: How should I test?\nAI: Use XCTest.\nUser: No, don't use XCTest, use Swift Testing instead.";
        let injected = vec![record("xctest-pattern", "Use XCTest framework for unit tests")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].signal, SignalType::Contradicted);
        assert_eq!(results[0].confidence_delta, -0.10);
        assert!(results[0].evidence.is_some());
    }

    #[test]
    fn test_contradiction_wrong() {
        let transcript = "The approach in the pattern is wrong, we should use async/await instead of callbacks.";
        let injected = vec![record("callback-pattern", "Use callbacks for async operations")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Contradicted);
    }

    #[test]
    fn test_contradiction_actually() {
        let transcript = "Actually, you should avoid using that deprecated API.";
        let injected = vec![record("old-api", "Use the legacy API for backward compatibility")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Contradicted);
    }

    #[test]
    fn test_contradiction_instead_of() {
        let transcript = "Instead of Redux, use Zustand for state management.";
        let injected = vec![record("redux-pattern", "Use Redux for global state management")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Contradicted);
    }

    // ─── Contradiction detection: Chinese ─────────────────────────

    #[test]
    fn test_contradiction_chinese_dont_use() {
        let transcript = "不要用 XCTest，改用 Swift Testing";
        let injected = vec![record("xctest-zh", "使用 XCTest 進行單元測試")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Contradicted);
    }

    #[test]
    fn test_contradiction_chinese_wrong() {
        let transcript = "這個做法錯了，應該用 async/await";
        let injected = vec![record("sync-pattern", "用同步方式處理 async 任務")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Contradicted);
    }

    #[test]
    fn test_contradiction_chinese_should_use() {
        let transcript = "應該用 SwiftUI 而不是 UIKit";
        let injected = vec![record("uikit-pattern", "使用 UIKit 建構界面")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Contradicted);
    }

    // ─── Reinforcement detection ──────────────────────────────────

    #[test]
    fn test_reinforcement_keywords_present() {
        let transcript = "I applied the @Test macro from Swift Testing and it worked great. The tests pass now.";
        let injected = vec![record("swift-testing", "Use @Test macro instead of XCTest assertions")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Reinforced);
        assert_eq!(results[0].confidence_delta, 0.03);
    }

    #[test]
    fn test_reinforcement_name_keywords() {
        let transcript = "Using the swift testing approach worked perfectly.";
        let injected = vec![record("swift-testing", "Prefer @Test for new test files")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Reinforced);
    }

    // ─── Ignored detection ────────────────────────────────────────

    #[test]
    fn test_ignored_no_reference() {
        let transcript = "Let's build a REST API with Express.js and add authentication.";
        let injected = vec![record("swift-testing", "Use @Test macro instead of XCTest")];
        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results[0].signal, SignalType::Ignored);
        assert_eq!(results[0].confidence_delta, -0.01);
    }

    // ─── Mixed session ────────────────────────────────────────────

    #[test]
    fn test_mixed_session() {
        let transcript = "\
User: How should I handle errors?
AI: Based on patterns, use Result<T, Error> and the error-handling crate.
User: Good, I'll use Result for errors.
User: But don't use that old logging approach, use tracing instead.
AI: Updated to use tracing for structured logging.";

        let injected = vec![
            record("error-handling", "Use Result<T, Error> for error propagation"),
            record("old-logging", "Use the log crate with env_logger"),
            record("unused-pattern", "Apply flexbox grid layout for responsive CSS design"),
        ];

        let results = analyze_session_feedback(transcript, &injected);
        assert_eq!(results.len(), 3);

        // error-handling should be reinforced
        assert_eq!(results[0].signal, SignalType::Reinforced);
        assert_eq!(results[0].confidence_delta, 0.03);

        // old-logging should be contradicted (don't use + logging keywords)
        assert_eq!(results[1].signal, SignalType::Contradicted);
        assert_eq!(results[1].confidence_delta, -0.10);

        // unused-pattern should be ignored
        assert_eq!(results[2].signal, SignalType::Ignored);
        assert_eq!(results[2].confidence_delta, -0.01);
    }

    // ─── Injection record roundtrip ───────────────────────────────

    #[test]
    fn test_injection_record_serde_roundtrip() {
        let record = InjectionRecord {
            timestamp: "2026-02-25T12:00:00Z".into(),
            query: "fix this bug".into(),
            project: "my-project".into(),
            patterns: vec![
                InjectedPatternRecord {
                    name: "swift-testing".into(),
                    snippet: "Use @Test macro instead of...".into(),
                },
            ],
        };

        let json = serde_json::to_string_pretty(&record).unwrap();
        let parsed: InjectionRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.timestamp, record.timestamp);
        assert_eq!(parsed.query, record.query);
        assert_eq!(parsed.project, record.project);
        assert_eq!(parsed.patterns.len(), 1);
        assert_eq!(parsed.patterns[0].name, "swift-testing");
        assert_eq!(parsed.patterns[0].snippet, "Use @Test macro instead of...");
    }

    #[test]
    fn test_injection_record_write_read_roundtrip() {
        // Use a temp dir to avoid polluting real ~/.mur/
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("last_injection.json");

        let record = InjectionRecord {
            timestamp: "2026-02-25T12:00:00Z".into(),
            query: "fix this bug".into(),
            project: "my-project".into(),
            patterns: vec![
                InjectedPatternRecord {
                    name: "test-pat".into(),
                    snippet: "Test snippet content".into(),
                },
            ],
        };

        // Write
        let json = serde_json::to_string_pretty(&record).unwrap();
        std::fs::write(&path, &json).unwrap();

        // Read
        let data = std::fs::read_to_string(&path).unwrap();
        let parsed: InjectionRecord = serde_json::from_str(&data).unwrap();
        assert_eq!(parsed.patterns[0].name, "test-pat");
    }

    // ─── Empty input ──────────────────────────────────────────────

    #[test]
    fn test_empty_injected_list() {
        let results = analyze_session_feedback("some transcript", &[]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_empty_transcript() {
        let injected = vec![record("pattern-a", "some pattern content here")];
        let results = analyze_session_feedback("", &injected);
        assert_eq!(results[0].signal, SignalType::Ignored);
    }

    // ─── Confidence delta bounds ──────────────────────────────────

    #[test]
    fn test_confidence_delta_never_exceeds_bounds() {
        // Simulate applying many feedbacks
        let mut confidence = 0.5_f64;

        // 100 reinforcements
        for _ in 0..100 {
            confidence = (confidence + 0.03).min(1.0);
        }
        assert!(confidence <= 1.0);

        // 100 contradictions
        confidence = 0.5;
        for _ in 0..100 {
            confidence = (confidence - 0.10).max(0.0);
        }
        assert!(confidence >= 0.0);

        // 100 ignores
        confidence = 0.5;
        for _ in 0..100 {
            confidence = (confidence - 0.01).max(0.0);
        }
        assert!(confidence >= 0.0);
    }

    // ─── Realistic transcript ─────────────────────────────────────

    #[test]
    fn test_realistic_claude_code_transcript() {
        let transcript = "\
Human: I need to add error handling to this Rust function
Assistant: I'll use the anyhow crate with Result types for clean propagation.
Human: Perfect, anyhow is exactly what we want.
Human: Also, don't use unwrap() directly in production code.
Assistant: Understood, I'll use the ? operator for safe unwinding.";

        let injected = vec![
            record("anyhow-errors", "Use anyhow::Result for application error handling"),
            record("unwrap-ok", "Use unwrap() for cases where None is impossible"),
        ];

        let results = analyze_session_feedback(transcript, &injected);

        // anyhow-errors: reinforced (keywords "anyhow", "result", "error" present)
        assert_eq!(results[0].signal, SignalType::Reinforced);

        // unwrap-ok: contradicted ("don't use" + "unwrap")
        assert_eq!(results[1].signal, SignalType::Contradicted);
    }
}