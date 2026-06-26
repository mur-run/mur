//! PKCE (RFC 7636) helpers and OAuth discovery-URL builders (RFC 8414 / 9728).
//!
//! Pure functions — no network, no I/O.

use base64::Engine as _;
use sha2::{Digest, Sha256};

// ── PKCE ────────────────────────────────────────────────────────────────────

/// Encode `seed` as a base64url (no padding) code verifier.
///
/// The caller is responsible for supplying 32 cryptographically random bytes.
/// Using a fixed seed is only appropriate for tests / deterministic vectors.
pub fn code_verifier(seed: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed)
}

/// Derive the PKCE code challenge from a verifier (S256 method, RFC 7636 §4.2).
///
/// `challenge = BASE64URL-NO-PAD(SHA256(ASCII(verifier)))`
pub fn code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
}

// ── Discovery URL builders ───────────────────────────────────────────────────

/// RFC 9728 §5 — Protected Resource Metadata URL.
///
/// `<origin>/.well-known/oauth-protected-resource`
pub fn protected_resource_url(server: &str) -> String {
    format!("{}/.well-known/oauth-protected-resource", origin_of(server))
}

/// RFC 8414 §3 — Authorization Server Metadata URL.
///
/// `<origin>/.well-known/oauth-authorization-server`
pub fn as_metadata_url(issuer: &str) -> String {
    format!(
        "{}/.well-known/oauth-authorization-server",
        origin_of(issuer)
    )
}

/// Extract the scheme+host+port from a URL — no `url` crate dependency.
///
/// // ponytail: string-split is sufficient; avoid pulling a URL crate for one fn.
fn origin_of(url: &str) -> &str {
    // Strip trailing slash, then find end of authority (third slash after scheme).
    let url = url.trim_end_matches('/');
    // scheme://host[:port][/path] — find the '//' then the next '/'
    if let Some(after_scheme) = url.find("://") {
        let rest_start = after_scheme + 3; // skip "://"
        let rest = &url[rest_start..];
        if let Some(path_start) = rest.find('/') {
            &url[..rest_start + path_start]
        } else {
            url // no path component → whole string is origin
        }
    } else {
        url // malformed — return as-is
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B test vector — the single authoritative correctness check.
    ///
    /// verifier  = dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk
    /// challenge = E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM
    #[test]
    fn pkce_matches_rfc7636_vector() {
        // The RFC verifier is itself a base64url string; feed its bytes as seed
        // so code_verifier round-trips to the same string.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = code_challenge(verifier);
        assert_eq!(
            challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "SHA-256/base64url challenge must match RFC 7636 Appendix B"
        );
    }

    #[test]
    fn discovery_urls() {
        assert_eq!(
            protected_resource_url("https://api.example.com/mcp"),
            "https://api.example.com/.well-known/oauth-protected-resource"
        );
        assert_eq!(
            as_metadata_url("https://auth.example.com"),
            "https://auth.example.com/.well-known/oauth-authorization-server"
        );
        // Trailing slash tolerance
        assert_eq!(
            protected_resource_url("https://api.example.com/"),
            "https://api.example.com/.well-known/oauth-protected-resource"
        );
        assert_eq!(
            as_metadata_url("https://auth.example.com/"),
            "https://auth.example.com/.well-known/oauth-authorization-server"
        );
    }
}
