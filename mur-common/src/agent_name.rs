//! Canonical agent-name validation. Used by both `mur agent create` (CLI) and
//! the `/api/v1/agents/{name}` HTTP routes so the two surfaces never disagree
//! on what names are valid.

use std::fmt;

pub const MAX_AGENT_NAME_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentNameError {
    Empty,
    TooLong { max: usize, actual: usize },
    InvalidChar(char),
    LeadingDash,
    ContainsTraversal,
}

impl fmt::Display for AgentNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "agent name must not be empty"),
            Self::TooLong { max, actual } => {
                write!(f, "agent name is {actual} chars; max is {max}")
            }
            Self::InvalidChar(c) => write!(
                f,
                "agent name contains invalid character {c:?} \
                 (allowed: ASCII letters, digits, '-', '_')"
            ),
            Self::LeadingDash => write!(
                f,
                "agent name must not start with '-' (parses as a CLI flag)"
            ),
            Self::ContainsTraversal => {
                write!(f, "agent name must not contain '..' (path traversal)")
            }
        }
    }
}

impl std::error::Error for AgentNameError {}

/// Validate an agent name from any caller (HTTP path param, CLI argument,
/// import file). Returns `Ok(())` if the name is safe to use as a directory
/// component under `~/.mur/agents/`.
pub fn validate_agent_name(name: &str) -> Result<(), AgentNameError> {
    if name.is_empty() {
        return Err(AgentNameError::Empty);
    }
    if name.len() > MAX_AGENT_NAME_LEN {
        return Err(AgentNameError::TooLong {
            max: MAX_AGENT_NAME_LEN,
            actual: name.len(),
        });
    }
    if name.starts_with('-') {
        return Err(AgentNameError::LeadingDash);
    }
    if name.contains("..") {
        return Err(AgentNameError::ContainsTraversal);
    }
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_') {
            return Err(AgentNameError::InvalidChar(c));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typical_names() {
        for name in ["alpha", "support_bot", "agent-1", "research_v2", "a", "X"] {
            assert!(
                validate_agent_name(name).is_ok(),
                "expected {name} to be valid"
            );
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(validate_agent_name(""), Err(AgentNameError::Empty));
    }

    #[test]
    fn rejects_traversal() {
        assert_eq!(
            validate_agent_name(".."),
            Err(AgentNameError::ContainsTraversal)
        );
        assert_eq!(
            validate_agent_name("a..b"),
            Err(AgentNameError::ContainsTraversal)
        );
    }

    #[test]
    fn rejects_path_separators_and_other_specials() {
        for bad in ["a/b", "a\\b", "a b", "a.b", "a:b", "a$b", "a@b", "a\0b"] {
            let err = validate_agent_name(bad).unwrap_err();
            // Specifically expect InvalidChar (not Empty / TooLong / etc.)
            assert!(
                matches!(err, AgentNameError::InvalidChar(_)),
                "expected InvalidChar for {bad:?}, got {err:?}",
            );
        }
    }

    #[test]
    fn rejects_unicode_lookalikes() {
        // Cyrillic 'а' (U+0430) vs ASCII 'a' (U+0061)
        assert!(matches!(
            validate_agent_name("аlpha").unwrap_err(),
            AgentNameError::InvalidChar(_)
        ));
    }

    #[test]
    fn rejects_leading_dash() {
        assert_eq!(validate_agent_name("-rf"), Err(AgentNameError::LeadingDash));
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(MAX_AGENT_NAME_LEN + 1);
        assert!(matches!(
            validate_agent_name(&long).unwrap_err(),
            AgentNameError::TooLong { .. }
        ));
    }

    #[test]
    fn accepts_max_length() {
        let max = "a".repeat(MAX_AGENT_NAME_LEN);
        assert!(validate_agent_name(&max).is_ok());
    }
}
