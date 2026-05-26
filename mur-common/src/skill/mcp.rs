//! MCP capability declaration vocabulary (M6a).
//!
//! Shared string-form contract between skill manifests and commander's
//! MCP trust store. `mur-common` owns the enum so the YAML schema is
//! stable even if commander refactors internally.
//!
//! If commander adds a 7th capability, add the variant + `as_str`/`FromStr`
//! arms here with no other code changes needed.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// MCP capabilities a skill may declare it requires. Mirrors the six
/// capabilities defined in mur-commander's `engine/src/mcp/trust.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkillCapability {
    ReadFile,
    ListTools,
    Search,
    WriteFile,
    ExecuteSafe,
    NetworkHttp,
}

impl SkillCapability {
    pub const ALL: &[SkillCapability] = &[
        SkillCapability::ReadFile,
        SkillCapability::ListTools,
        SkillCapability::Search,
        SkillCapability::WriteFile,
        SkillCapability::ExecuteSafe,
        SkillCapability::NetworkHttp,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SkillCapability::ReadFile => "read_file",
            SkillCapability::ListTools => "list_tools",
            SkillCapability::Search => "search",
            SkillCapability::WriteFile => "write_file",
            SkillCapability::ExecuteSafe => "execute_safe",
            SkillCapability::NetworkHttp => "network_http",
        }
    }
}

impl std::fmt::Display for SkillCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SkillCapability {
    type Err = ParseCapabilityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "read_file" => SkillCapability::ReadFile,
            "list_tools" => SkillCapability::ListTools,
            "search" => SkillCapability::Search,
            "write_file" => SkillCapability::WriteFile,
            "execute_safe" => SkillCapability::ExecuteSafe,
            "network_http" => SkillCapability::NetworkHttp,
            other => return Err(ParseCapabilityError(other.to_string())),
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "unknown MCP capability '{0}' (expected one of: read_file, list_tools, search, \
     write_file, execute_safe, network_http)"
)]
pub struct ParseCapabilityError(pub String);

// Serde uses the string form so YAML reads as `capability: read_file`,
// not `capability: ReadFile`.
impl Serialize for SkillCapability {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SkillCapability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A requirement that one or more MCP tools matching `tool_pattern` be
/// reachable at runtime, and that they are safe to invoke under `capability`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpRequirement {
    /// Glob pattern matching tool names, e.g. `"browser.*"` or
    /// `"filesystem.write.*"`.
    pub tool_pattern: String,

    /// Capability for the matched tool. Used by commander's trust store
    /// at runtime (M6b) to decide whether to permit the call.
    pub capability: SkillCapability,

    /// Optional fallback. Empty string means "no fallback — fail if no
    /// matching tool is available". Free-form; not validated here.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fallback: String,
}

/// Validate a list of requirements at parse time.
///
/// Checks: non-empty pattern, valid glob syntax, no duplicate
/// (pattern, capability) pairs.
pub fn validate_requirements(reqs: &[McpRequirement]) -> Result<(), (usize, String)> {
    use std::collections::HashSet;
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    for (i, req) in reqs.iter().enumerate() {
        if req.tool_pattern.is_empty() {
            return Err((i, "tool_pattern must not be empty".into()));
        }
        // Validate glob syntax.
        if globset::Glob::new(&req.tool_pattern).is_err() {
            return Err((i, format!("invalid glob pattern: '{}'", req.tool_pattern)));
        }
        let cap_str = req.capability.as_str();
        if !seen.insert((&req.tool_pattern, cap_str)) {
            return Err((
                i,
                format!(
                    "duplicate (tool_pattern, capability) pair: '{}'/{}",
                    req.tool_pattern, req.capability
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_capabilities() {
        assert_eq!(
            "read_file".parse::<SkillCapability>().unwrap(),
            SkillCapability::ReadFile
        );
        assert_eq!(
            "list_tools".parse::<SkillCapability>().unwrap(),
            SkillCapability::ListTools
        );
        assert_eq!(
            "search".parse::<SkillCapability>().unwrap(),
            SkillCapability::Search
        );
        assert_eq!(
            "write_file".parse::<SkillCapability>().unwrap(),
            SkillCapability::WriteFile
        );
        assert_eq!(
            "execute_safe".parse::<SkillCapability>().unwrap(),
            SkillCapability::ExecuteSafe
        );
        assert_eq!(
            "network_http".parse::<SkillCapability>().unwrap(),
            SkillCapability::NetworkHttp
        );
    }

    #[test]
    fn parse_invalid_capability() {
        let err = "telepathy".parse::<SkillCapability>().unwrap_err();
        assert!(err.to_string().contains("unknown MCP capability"));
        assert!(err.to_string().contains("telepathy"));
    }

    #[test]
    fn capability_display_roundtrips() {
        for cap in SkillCapability::ALL {
            let s = cap.to_string();
            let parsed: SkillCapability = s.parse().unwrap();
            assert_eq!(parsed, *cap);
        }
    }

    #[test]
    fn capability_serializes_as_string() {
        let json = serde_json::to_string(&SkillCapability::ReadFile).unwrap();
        assert_eq!(json, "\"read_file\"");
    }

    #[test]
    fn capability_deserializes_from_string() {
        let cap: SkillCapability = serde_json::from_str("\"network_http\"").unwrap();
        assert_eq!(cap, SkillCapability::NetworkHttp);
    }

    #[test]
    fn capability_rejects_unknown_string() {
        let err = serde_json::from_str::<SkillCapability>("\"telepathy\"").unwrap_err();
        assert!(err.to_string().contains("unknown MCP capability"));
    }

    #[test]
    fn validate_empty_requirements() {
        assert!(validate_requirements(&[]).is_ok());
    }

    #[test]
    fn validate_single_requirement() {
        let reqs = vec![McpRequirement {
            tool_pattern: "browser.*".into(),
            capability: SkillCapability::NetworkHttp,
            fallback: String::new(),
        }];
        assert!(validate_requirements(&reqs).is_ok());
    }

    #[test]
    fn validate_rejects_empty_pattern() {
        let reqs = vec![McpRequirement {
            tool_pattern: String::new(),
            capability: SkillCapability::ReadFile,
            fallback: String::new(),
        }];
        let err = validate_requirements(&reqs).unwrap_err();
        assert_eq!(err.0, 0);
        assert!(err.1.contains("tool_pattern must not be empty"));
    }

    #[test]
    fn validate_rejects_duplicate() {
        let reqs = vec![
            McpRequirement {
                tool_pattern: "browser.*".into(),
                capability: SkillCapability::NetworkHttp,
                fallback: String::new(),
            },
            McpRequirement {
                tool_pattern: "browser.*".into(),
                capability: SkillCapability::NetworkHttp,
                fallback: String::new(),
            },
        ];
        let err = validate_requirements(&reqs).unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("duplicate"));
    }

    #[test]
    fn validate_allows_same_pattern_different_capability() {
        let reqs = vec![
            McpRequirement {
                tool_pattern: "fs.*".into(),
                capability: SkillCapability::ReadFile,
                fallback: String::new(),
            },
            McpRequirement {
                tool_pattern: "fs.*".into(),
                capability: SkillCapability::WriteFile,
                fallback: String::new(),
            },
        ];
        assert!(validate_requirements(&reqs).is_ok());
    }
}
