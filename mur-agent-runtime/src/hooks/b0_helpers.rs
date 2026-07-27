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

/// Match a host string against an allowlist.
///
/// Delegates to [`crate::sandbox::reqwest_guard::host_matches_pattern`] so
/// both the B0 gate and the HostGuard DNS resolver share a single wildcard
/// interpretation.  The canonical form is `*.example.com`; the legacy
/// `.example.com` leading-dot form is also accepted.
pub fn host_is_allowlisted(host: &str, allow: &[String]) -> bool {
    allow
        .iter()
        .any(|p| crate::sandbox::reqwest_guard::host_matches_pattern(host, p))
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
/// match's classification (or `None` if clean). Delegates to
/// `mur_common::skill::scan::secrets` so the pattern list is a single
/// source of truth.
pub fn scan_for_secrets(body: &str) -> Option<&'static str> {
    mur_common::skill::scan::scan_secrets(body)
        .into_iter()
        .next()
        .map(|f| f.label)
}

/// Replace every match of the credential pattern set with
/// `[REDACTED:<label>]`. Used at the telemetry write boundary
/// (B0 rule 9 / M8.1) to scrub free-form strings before they
/// land on disk in `~/.mur/agents/<name>/telemetry/<date>.jsonl`.
///
/// Returns `Cow::Borrowed` when nothing matched so the common
/// hot path (no secrets present) avoids any allocation.
pub fn redact_secrets(input: &str) -> std::borrow::Cow<'_, str> {
    // TODO(M1): collapse into mur-common::skill::scan::secrets
    use regex::Regex;
    use std::borrow::Cow;
    use std::sync::OnceLock;

    static REDACT_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = REDACT_PATTERNS.get_or_init(|| {
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

    let mut out: Cow<'_, str> = Cow::Borrowed(input);
    for (rx, label) in patterns {
        if rx.is_match(&out) {
            let replacement = format!("[REDACTED:{label}]");
            out = Cow::Owned(rx.replace_all(&out, replacement.as_str()).into_owned());
        }
    }
    out
}

/// Replace home-directory-style absolute paths with `~/`. Catches
/// macOS `/Users/<user>/`, Linux `/home/<user>/`, and Windows
/// `C:\Users\<user>\` so error messages don't leak the OS user
/// account name in telemetry. Conservative: only the username
/// portion is collapsed; the trailing path is preserved so
/// debugging context survives.
pub fn redact_home_path(input: &str) -> std::borrow::Cow<'_, str> {
    use regex::Regex;
    use std::borrow::Cow;
    use std::sync::OnceLock;

    static RE_UNIX: OnceLock<Regex> = OnceLock::new();
    static RE_MAC: OnceLock<Regex> = OnceLock::new();
    static RE_WIN: OnceLock<Regex> = OnceLock::new();

    let unix = RE_UNIX.get_or_init(|| Regex::new(r"/home/[^/\s]+/").unwrap());
    let mac = RE_MAC.get_or_init(|| Regex::new(r"/Users/[^/\s]+/").unwrap());
    let win = RE_WIN.get_or_init(|| Regex::new(r"(?i)[A-Z]:\\Users\\[^\\\s]+\\").unwrap());

    let mut out: Cow<'_, str> = Cow::Borrowed(input);
    for rx in [unix, mac] {
        if rx.is_match(&out) {
            out = Cow::Owned(rx.replace_all(&out, "~/").into_owned());
        }
    }
    if win.is_match(&out) {
        out = Cow::Owned(win.replace_all(&out, "~\\").into_owned());
    }
    out
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

#[cfg(test)]
mod telemetry_redact_tests {
    use super::*;

    #[test]
    fn redact_secrets_replaces_openai_key() {
        let out = redact_secrets("oops sk-abcd1234567890efghij1234 leaked");
        assert!(out.contains("[REDACTED:openai_key]"), "got {out:?}");
        assert!(!out.contains("sk-abcd"));
    }

    #[test]
    fn redact_secrets_replaces_anthropic_and_aws_in_one_string() {
        let s = "key1 sk-ant-abcdefghijklmnop-9999 and aws AKIAIOSFODNN7EXAMPL2";
        let out = redact_secrets(s);
        assert!(out.contains("[REDACTED:anthropic_key]"));
        assert!(out.contains("[REDACTED:aws_access_key]"));
    }

    #[test]
    fn redact_secrets_clean_text_borrows() {
        // No allocation when input is clean.
        let s = "all good here, no secrets";
        let out = redact_secrets(s);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn redact_home_path_collapses_macos() {
        let out = redact_home_path("failed to read /Users/alice/secret.txt: nope");
        assert_eq!(out, "failed to read ~/secret.txt: nope");
    }

    #[test]
    fn redact_home_path_collapses_linux() {
        let out = redact_home_path("ENOENT at /home/bob/.ssh/id_rsa");
        assert_eq!(out, "ENOENT at ~/.ssh/id_rsa");
    }

    #[test]
    fn redact_home_path_collapses_windows() {
        let out = redact_home_path(r"open C:\Users\Carol\Desktop\notes.md failed");
        assert!(out.contains(r"~\Desktop\notes.md"), "got {out:?}");
    }

    #[test]
    fn redact_home_path_clean_text_borrows() {
        let s = "no path here";
        let out = redact_home_path(s);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn redact_secrets_handles_pem_block() {
        let out = redact_secrets("-----BEGIN RSA PRIVATE KEY-----\nMIIE...");
        assert!(out.contains("[REDACTED:pem_private_key]"), "got {out:?}");
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

// ─────────────────────────────────────────────────────────────────
// B0 rule 6 / M9.3 — MCP install-time pin verification.
// ─────────────────────────────────────────────────────────────────

/// Why a pinned MCP entry failed re-verification at startup. Used to
/// drive the user-facing recovery prompt (`mur agent mcp inspect
/// <name>`) so the message can specifically say "the binary changed"
/// vs. "the description changed" rather than a generic mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinDriftReason {
    /// The pinned binary path could not be resolved on disk. The user
    /// either uninstalled the MCP without removing it from
    /// profile.yaml or the binary moved. Treated as a soft fail (warn,
    /// don't block) so the supervisor can still start.
    BinaryMissing { path: String, io_error: String },
    /// The pinned binary's SHA-256 did not match the recorded hash.
    /// Hard fail — the binary on disk is not what was approved at
    /// install time.
    BinaryDrift { expected: String, actual: String },
    /// I/O error while reading the binary (permissions / disk).
    /// Treated as soft fail; rule 11 codesign check would have caught
    /// the equivalent permission issue first.
    BinaryReadError { path: String, io_error: String },
}

impl std::fmt::Display for PinDriftReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BinaryMissing { path, io_error } => {
                write!(f, "binary at `{path}` cannot be located ({io_error})")
            }
            Self::BinaryDrift { expected, actual } => {
                let exp_short = expected.chars().take(16).collect::<String>();
                let act_short = actual.chars().take(16).collect::<String>();
                write!(
                    f,
                    "binary SHA-256 changed (pinned: {exp_short}…, current: {act_short}…)"
                )
            }
            Self::BinaryReadError { path, io_error } => {
                write!(f, "binary at `{path}` could not be read ({io_error})")
            }
        }
    }
}

/// Re-compute the SHA-256 of `path` and compare with `expected`. Used
/// by `B0SafetyHook::on_startup` to enforce the rule-6 install-time
/// pin on every supervisor start.
///
/// Mirrors the chunked stream-hash in `mur-core::cmd::agent_mcp_pin`
/// so the install-side and verify-side outputs are byte-identical.
/// Duplicated rather than imported to avoid the `mur-agent-runtime ←
/// mur-core` dependency cycle this hook already takes pains to avoid.
/// SHA-256 (lowercase hex) of a binary, in exactly the form a `binary_sha256`
/// pin records.
///
/// Shared by the verifier below and by the supervisor's re-pin of MUR's own
/// bundled MCP server, so the hash that gets written and the hash that gets
/// checked can never disagree about algorithm or encoding.
pub fn binary_sha256(path: &std::path::Path) -> Result<String, PinDriftReason> {
    use sha2::{Digest, Sha256};
    use std::fs::File;
    use std::io::Read;

    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            // ENOENT vs other I/O — distinguish so the user sees a
            // clear "uninstalled?" hint rather than a generic error.
            if e.kind() == std::io::ErrorKind::NotFound {
                return Err(PinDriftReason::BinaryMissing {
                    path: path.display().to_string(),
                    io_error: e.to_string(),
                });
            }
            return Err(PinDriftReason::BinaryReadError {
                path: path.display().to_string(),
                io_error: e.to_string(),
            });
        }
    };

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(PinDriftReason::BinaryReadError {
                    path: path.display().to_string(),
                    io_error: e.to_string(),
                });
            }
        };
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn verify_mcp_binary_hash(
    expected: &str,
    path: &std::path::Path,
) -> Result<(), PinDriftReason> {
    let actual = binary_sha256(path)?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(PinDriftReason::BinaryDrift {
            expected: expected.to_string(),
            actual,
        })
    }
}

#[cfg(test)]
mod pin_verify_tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Same SHA-256 fixture as `mur-core::cmd::agent_mcp_pin` so the
    /// two halves of M9 stay in lockstep.
    const HELLO_SHA: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

    #[test]
    fn matches_pinned_hash_returns_ok() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello\n").unwrap();
        assert!(verify_mcp_binary_hash(HELLO_SHA, f.path()).is_ok());
    }

    #[test]
    fn case_insensitive_hash_match() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello\n").unwrap();
        let upper: String = HELLO_SHA.chars().map(|c| c.to_ascii_uppercase()).collect();
        assert!(verify_mcp_binary_hash(&upper, f.path()).is_ok());
    }

    #[test]
    fn drift_returns_specific_reason() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"world\n").unwrap();
        let err = verify_mcp_binary_hash(HELLO_SHA, f.path()).unwrap_err();
        match err {
            PinDriftReason::BinaryDrift { expected, actual } => {
                assert_eq!(expected, HELLO_SHA);
                assert_ne!(actual, HELLO_SHA);
                assert_eq!(actual.len(), 64);
            }
            other => panic!("expected BinaryDrift, got {other:?}"),
        }
    }

    #[test]
    fn missing_binary_distinguished_from_read_error() {
        let err = verify_mcp_binary_hash(HELLO_SHA, std::path::Path::new("/no/such/binary-xyz"))
            .unwrap_err();
        assert!(
            matches!(err, PinDriftReason::BinaryMissing { .. }),
            "got {err:?}",
        );
    }

    #[test]
    fn drift_display_includes_short_hash_prefixes() {
        let drift = PinDriftReason::BinaryDrift {
            expected: "deadbeef00112233445566778899aabbccddeeff00112233445566778899aabb".into(),
            actual: "cafebabe00112233445566778899aabbccddeeff00112233445566778899aabb".into(),
        };
        let s = drift.to_string();
        assert!(s.contains("deadbeef00112233"), "got {s}");
        assert!(s.contains("cafebabe00112233"), "got {s}");
        assert!(!s.contains("deadbeef00112233445566"), "no full hash leak");
    }
}
