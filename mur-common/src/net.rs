//! Shared host-pattern matcher for outbound-network allow/deny checks.
//!
//! This is the single source of truth for host-pattern matching used by every
//! layer that gates outbound requests: the agent runtime's egress proxy
//! (`mur-agent-runtime::sandbox::reqwest_guard`) and the research gateway's
//! SSRF guard (`mur-research-gateway::net_guard`). A security boundary must
//! not have two copies of this logic that can silently drift apart.
//!
//! IP-range predicates (loopback/private/link-local/unspecified) are
//! deliberately NOT shared here — different callers apply different IP
//! policies (e.g. the runtime allows loopback/RFC1918 for local LLMs while
//! the research gateway forbids them). Only the host-pattern string matcher
//! is common.

/// Match a host against an allowlist pattern.
///
/// Canonical wildcard syntax is `*.example.com` (matches `api.example.com`
/// and `example.com`).  For backward compatibility the legacy leading-dot
/// form `.example.com` is also accepted and treated identically.
///
/// Both layers that perform host-allowlist checks (`HostGuard` DNS resolver
/// and the B0 safety hook) must call this function so they share a single
/// interpretation of wildcard patterns.
pub fn host_matches_pattern(host: &str, pattern: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    // Strip leading `*.` (canonical) or leading `.` (legacy) to get the suffix.
    let suffix = if let Some(s) = pattern.strip_prefix("*.") {
        s
    } else if let Some(s) = pattern.strip_prefix('.') {
        s
    } else {
        // Exact match only.
        return host == pattern;
    };
    host == suffix || host.ends_with(&format!(".{suffix}"))
}

/// True if `host` matches any allowlist pattern. An empty allowlist denies all
/// (fail-closed). Reuses the same matcher the agent's reqwest guard uses, so
/// per-MCP-server egress allowlisting behaves identically to agent-level hosts.
pub fn host_allowed(host: &str, allow: &[String]) -> bool {
    allow.iter().any(|p| host_matches_pattern(host, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_matches_pattern_wildcard_legacy_and_exact() {
        assert!(host_matches_pattern("api.example.com", "*.example.com"));
        assert!(host_matches_pattern("example.com", "*.example.com"));
        assert!(host_matches_pattern("api.example.com", ".example.com"));
        assert!(host_matches_pattern("example.com", ".example.com"));
        assert!(host_matches_pattern("example.com", "example.com"));
        assert!(!host_matches_pattern("evil.com", "example.com"));
        assert!(!host_matches_pattern(
            "example.com.evil.com",
            "*.example.com"
        ));
    }

    #[test]
    fn host_allowed_is_fail_closed_and_pattern_aware() {
        let allow = vec!["example.com".to_string(), "*.api.example.com".to_string()];
        assert!(host_allowed("example.com", &allow));
        assert!(host_allowed("v1.api.example.com", &allow));
        assert!(!host_allowed("evil.com", &allow));
        assert!(!host_allowed("example.com", &[]), "empty allowlist denies");
    }
}
