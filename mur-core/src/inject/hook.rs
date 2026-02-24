//! Hook/inject: format patterns for injection into AI tool prompts.

use mur_common::pattern::{Content, Pattern};

/// When to trigger pattern retrieval
#[derive(Debug, Clone, PartialEq)]
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
