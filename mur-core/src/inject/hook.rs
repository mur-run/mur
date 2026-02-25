//! Hook/inject: format patterns for injection into AI tool prompts.

use mur_common::pattern::{Content, Pattern};

/// When to trigger pattern retrieval
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // Manual variant used by CLI callers
pub enum HookTrigger {
    /// Beginning of AI session
    SessionStart,
    /// Error/failure detected in session
    OnError,
    /// Repeated attempt detected
    OnRetry,
    /// Manual: `mur inject --query "..."`
    Manual,
}

/// Detect what kind of hook trigger a message represents.
pub fn detect_trigger(message: &str) -> HookTrigger {
    let lower = message.to_lowercase();

    // Error indicators
    let error_keywords = [
        "error", "fail", "crash", "panic", "exception", "traceback",
        "segfault", "abort", "undefined", "cannot find", "not found",
        "permission denied", "timeout", "refused", "broken",
        "錯誤", "失敗", "崩潰",
    ];
    for kw in &error_keywords {
        if lower.contains(kw) {
            return HookTrigger::OnError;
        }
    }

    // Retry indicators
    let retry_keywords = [
        "again", "retry", "try again", "still not", "same error",
        "still failing", "didn't work", "not working",
        "再試", "還是不行", "一樣的問題",
    ];
    for kw in &retry_keywords {
        if lower.contains(kw) {
            return HookTrigger::OnRetry;
        }
    }

    HookTrigger::SessionStart
}

/// Format scored patterns for injection into a prompt.
/// Returns content only (no metadata), within token budget.
pub fn format_for_injection(patterns: &[Pattern], max_tokens: usize) -> String {
    if patterns.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Relevant patterns from your learning history\n\n");
    let mut token_count = output.len() / 4; // rough estimate

    for (i, pattern) in patterns.iter().enumerate() {
        let entry = format_pattern_entry(pattern, i + 1);
        let entry_tokens = entry.len() / 4;

        if token_count + entry_tokens > max_tokens && i > 0 {
            break;
        }

        output.push_str(&entry);
        output.push('\n');
        token_count += entry_tokens;
    }

    output.trim_end().to_string()
}

fn format_pattern_entry(pattern: &Pattern, index: usize) -> String {
    let content = match &pattern.content {
        Content::DualLayer {
            technical,
            principle,
        } => {
            let mut s = technical.clone();
            if let Some(p) = principle {
                s.push_str("\n💡 ");
                s.push_str(p);
            }
            s
        }
        Content::Plain(s) => s.clone(),
    };

    format!(
        "### {}. {}\n{}\n",
        index,
        pattern.description,
        content.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::*;

    fn make_pattern(desc: &str, content: &str) -> Pattern {
        Pattern {
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
        }
    }

    #[test]
    fn test_empty_patterns() {
        assert_eq!(format_for_injection(&[], 2000), "");
    }

    #[test]
    fn test_single_pattern() {
        let p = make_pattern("Use Swift Testing", "Use @Test macro");
        let result = format_for_injection(&[p], 2000);
        assert!(result.contains("Use Swift Testing"));
        assert!(result.contains("@Test macro"));
    }

    #[test]
    fn test_dual_layer() {
        let p = Pattern {
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
        };
        let result = format_for_injection(&[p], 2000);
        assert!(result.contains("Do X"));
        assert!(result.contains("💡 Because Y"));
    }

    #[test]
    fn test_detect_error_trigger() {
        assert_eq!(detect_trigger("I got an error: cannot find module"), HookTrigger::OnError);
        assert_eq!(detect_trigger("Build failed with exit code 1"), HookTrigger::OnError);
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
        assert_eq!(detect_trigger("Build a REST API for users"), HookTrigger::SessionStart);
        assert_eq!(detect_trigger("Refactor the auth module"), HookTrigger::SessionStart);
    }

    #[test]
    fn test_token_budget() {
        let patterns: Vec<Pattern> = (0..20)
            .map(|i| make_pattern(&format!("Pattern {}", i), &"x".repeat(500)))
            .collect();
        let result = format_for_injection(&patterns, 500);
        // Should not include all 20 patterns
        let count = result.matches("###").count();
        assert!(count < 20);
    }
}
