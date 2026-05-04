//! Pure helpers for B0SafetyHook rule branches.
//!
//! Each helper is a free function with no IO and no Tauri/runtime
//! state, so unit tests can construct fixtures directly. The helpers
//! are imported by `mur-agent-runtime/src/hooks/b0.rs` from the rule
//! branches that need them.

use std::path::Path;

/// Returns `true` when `candidate` is inside `confine_to` (after
/// canonicalization). A `candidate` that does NOT exist is checked
/// against the parent's canonical path — useful for fs.write where
/// the file may be about to be created.
///
/// Symlinks ARE followed (`canonicalize` resolves them) so this is a
/// real-world confinement check, not a string-prefix match.
pub fn path_confined_to(candidate: &Path, confine_to: &Path) -> bool {
    let confine_canonical = match std::fs::canonicalize(confine_to) {
        Ok(p) => p,
        Err(_) => return false, // confine_to missing — fail closed
    };
    let candidate_canonical = match std::fs::canonicalize(candidate) {
        Ok(p) => p,
        Err(_) => {
            // Not yet created. Check the parent.
            match candidate.parent() {
                Some(parent) => match std::fs::canonicalize(parent) {
                    Ok(p) => p,
                    Err(_) => return false,
                },
                None => return false,
            }
        }
    };
    candidate_canonical.starts_with(&confine_canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn confined_path_is_inside() {
        let dir = TempDir::new().unwrap();
        let inner = dir.path().join("inside.txt");
        std::fs::write(&inner, "x").unwrap();
        assert!(path_confined_to(&inner, dir.path()));
    }

    #[test]
    fn outside_path_rejected() {
        let dir = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let foreign = other.path().join("file.txt");
        std::fs::write(&foreign, "x").unwrap();
        assert!(!path_confined_to(&foreign, dir.path()));
    }

    #[test]
    fn nonexistent_file_uses_parent_for_check() {
        let dir = TempDir::new().unwrap();
        let new_file = dir.path().join("doesnt-exist-yet.txt");
        // Parent (dir) exists and IS the confine root.
        assert!(path_confined_to(&new_file, dir.path()));
    }

    #[test]
    fn nonexistent_parent_fails_closed() {
        let dir = TempDir::new().unwrap();
        let two_deep = dir.path().join("ghost-dir/file.txt");
        assert!(!path_confined_to(&two_deep, dir.path()));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_outside_rejected() {
        let confine = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let target = other.path().join("real.txt");
        std::fs::write(&target, "x").unwrap();
        let link = confine.path().join("escape.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        // Symlink resolves outside confine_to → reject.
        assert!(!path_confined_to(&link, confine.path()));
    }
}

/// Match a host string against an allowlist that supports leading-dot
/// wildcards (`.example.com` matches `api.example.com` and
/// `example.com`). Exact match also passes.
pub fn host_is_allowlisted(host: &str, allow: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    for pattern in allow {
        let pattern = pattern.to_ascii_lowercase();
        if let Some(suffix) = pattern.strip_prefix('.') {
            if host == suffix || host.ends_with(&format!(".{suffix}")) {
                return true;
            }
        } else if host == pattern {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod allowlist_tests {
    use super::*;

    #[test]
    fn exact_match_allowed() {
        assert!(host_is_allowlisted(
            "api.openai.com",
            &["api.openai.com".into()]
        ));
    }

    #[test]
    fn case_insensitive() {
        assert!(host_is_allowlisted(
            "API.OpenAI.com",
            &["api.openai.com".into()]
        ));
    }

    #[test]
    fn dot_prefix_matches_subdomain() {
        let allow = vec![".openai.com".into()];
        assert!(host_is_allowlisted("api.openai.com", &allow));
        assert!(host_is_allowlisted("openai.com", &allow));
    }

    #[test]
    fn unrelated_host_rejected() {
        let allow = vec![".openai.com".into()];
        assert!(!host_is_allowlisted("evil.com", &allow));
        assert!(!host_is_allowlisted("notopenai.com", &allow));
    }

    #[test]
    fn empty_allowlist_rejects_everything() {
        assert!(!host_is_allowlisted("api.openai.com", &[]));
    }
}

/// Scan body for known credential/secret patterns. Returns the FIRST
/// match's classification (or `None` if clean). Patterns deliberately
/// favor false-positives over false-negatives — accidentally dropping
/// a benign message is fine; leaking a key is not.
pub fn scan_for_secrets(body: &str) -> Option<&'static str> {
    use regex::Regex;
    use std::sync::OnceLock;

    // Compile once; share across calls.
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            // OpenAI / Anthropic API keys
            (Regex::new(r"\bsk-[a-zA-Z0-9]{20,}\b").unwrap(), "openai_key"),
            (Regex::new(r"\bsk-ant-[a-zA-Z0-9-]{20,}\b").unwrap(), "anthropic_key"),
            // AWS access keys
            (Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(), "aws_access_key"),
            (Regex::new(r"\baws_secret_access_key\s*[:=]\s*[A-Za-z0-9/+=]{40}\b").unwrap(), "aws_secret_key"),
            // GitHub PAT
            (Regex::new(r"\bghp_[A-Za-z0-9]{36}\b").unwrap(), "github_pat"),
            (Regex::new(r"\bghs_[A-Za-z0-9]{36}\b").unwrap(), "github_app_token"),
            // GCP service account / API key
            (Regex::new(r"\bAIza[0-9A-Za-z_-]{35}\b").unwrap(), "gcp_api_key"),
            // JWT (3 base64url segments separated by dots)
            (Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b").unwrap(), "jwt"),
            // PEM private key
            (Regex::new(r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----").unwrap(), "pem_private_key"),
            // Slack webhook
            (Regex::new(r"\bhooks\.slack\.com/services/T[A-Z0-9]+/B[A-Z0-9]+/[A-Za-z0-9]+\b").unwrap(), "slack_webhook"),
            // Generic .env-style assignment with high-entropy value
            (Regex::new(r"(?i)\b(api_key|api_secret|secret_key|access_token|password|token)\s*[:=]\s*[A-Za-z0-9_\-./+=]{20,}\b").unwrap(), "env_assignment"),
        ]
    });

    for (rx, label) in patterns {
        if rx.is_match(body) {
            return Some(label);
        }
    }
    None
}

#[cfg(test)]
mod secret_tests {
    use super::*;

    #[test]
    fn detects_openai_key() {
        assert_eq!(
            scan_for_secrets("here is my key: sk-abcd1234567890efghij1234"),
            Some("openai_key"),
        );
    }

    #[test]
    fn detects_anthropic_key() {
        assert!(scan_for_secrets("sk-ant-abcdefghijklmnopqrst-1234").is_some());
    }

    #[test]
    fn detects_aws_access_key() {
        assert_eq!(
            scan_for_secrets("AKIAIOSFODNN7EXAMPLE"),
            Some("aws_access_key"),
        );
    }

    #[test]
    fn detects_github_pat() {
        assert_eq!(
            scan_for_secrets("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            Some("github_pat"),
        );
    }

    #[test]
    fn detects_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4ifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert_eq!(scan_for_secrets(jwt), Some("jwt"));
    }

    #[test]
    fn detects_pem() {
        assert_eq!(
            scan_for_secrets("-----BEGIN RSA PRIVATE KEY-----\nMIIE...\n-----END..."),
            Some("pem_private_key"),
        );
    }

    #[test]
    fn detects_env_assignment() {
        assert_eq!(
            scan_for_secrets("api_key=abcdefghij1234567890"),
            Some("env_assignment"),
        );
    }

    #[test]
    fn clean_text_returns_none() {
        assert_eq!(scan_for_secrets("the model is gpt-4o today"), None);
        assert_eq!(scan_for_secrets("this is a normal message"), None);
    }
}

/// Redact common PII patterns in `body`. Returns the redacted text;
/// the redaction is permissive (catches obvious patterns; defers to
/// the user for ambiguous cases).
///
/// Replaces matched spans with `<REDACTED:label>`.
pub fn redact_pii(body: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        vec![
            // Email
            (
                Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b").unwrap(),
                "email",
            ),
            // US SSN
            (Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(), "ssn"),
            // Credit card (very loose: 13-19 digits in groups)
            (Regex::new(r"\b(?:\d{4}[- ]?){3,4}\d{1,4}\b").unwrap(), "cc"),
            // Phone (international or US-style)
            (
                Regex::new(r"\b\+?\d{1,3}[- ]?\(?\d{3}\)?[- ]?\d{3}[- ]?\d{4}\b").unwrap(),
                "phone",
            ),
        ]
    });

    let mut out = body.to_string();
    for (rx, label) in patterns {
        out = rx
            .replace_all(&out, format!("<REDACTED:{label}>"))
            .to_string();
    }
    out
}

#[cfg(test)]
mod redact_tests {
    use super::*;

    #[test]
    fn redacts_email() {
        assert_eq!(
            redact_pii("contact alex@example.com"),
            "contact <REDACTED:email>"
        );
    }

    #[test]
    fn redacts_ssn() {
        assert_eq!(redact_pii("ssn 123-45-6789"), "ssn <REDACTED:ssn>");
    }

    #[test]
    fn redacts_credit_card() {
        let red = redact_pii("card 4111-1111-1111-1111");
        assert!(red.contains("<REDACTED:cc>"), "got {red}");
    }

    #[test]
    fn redacts_phone() {
        let red = redact_pii("call +1-555-123-4567");
        assert!(red.contains("<REDACTED:phone>"), "got {red}");
    }

    #[test]
    fn clean_text_unchanged() {
        let clean = "the project will ship next week.";
        assert_eq!(redact_pii(clean), clean);
    }
}

/// Returns Ok(()) if the binary at `path` is signed (or sig-checks
/// don't apply on this platform). Returns Err with a user-actionable
/// reason on macOS/Windows when the signature is missing or invalid.
pub fn verify_signed(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("binary missing: {}", path.display()));
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/codesign")
            .args(["-dv", "--verbose=4"])
            .arg(path)
            .output()
            .map_err(|e| format!("codesign spawn: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "macOS binary not signed: {} (run `codesign -dv --verbose=4 {0}` for details)",
                path.display()
            ));
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        let out = std::process::Command::new("signtool")
            .args(["verify", "/pa", "/q"])
            .arg(path)
            .output()
            .map_err(|e| format!("signtool spawn: {e}"))?;
        if !out.status.success() {
            return Err(format!("Windows binary not signed: {}", path.display()));
        }
        Ok(())
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        // Linux: signing is not standard for native binaries.
        // Spec calls this out as macOS/Windows only.
        let _ = path;
        Ok(())
    }
}
