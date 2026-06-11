//! Secret scrubbing for session transcripts before cloud push.
//!
//! Three-layer detection:
//! 1. Prefix-based patterns (vendor-specific API keys)
//! 2. Contextual heuristics (.env files, PEM blocks, connection strings)
//! 3. High-entropy string detection (random API keys without known prefixes)
//!
//! Only applied before cloud upload — local `.jsonl` files are never modified.

use regex::Regex;
use std::sync::OnceLock;

use super::SessionEvent;

/// Scrub sensitive data from session events before cloud push.
/// Returns new events with redacted content. Original events are not modified.
pub fn scrub_events(events: &[SessionEvent]) -> Vec<SessionEvent> {
    events
        .iter()
        .map(|e| SessionEvent {
            timestamp: e.timestamp,
            event_type: e.event_type.clone(),
            tool: e.tool.clone(),
            content: scrub_content(&e.content, &e.event_type),
            working_dir: e.working_dir.clone(),
            git_branch: e.git_branch.clone(),
            exit_code: e.exit_code,
        })
        .collect()
}

/// Count how many secrets would be redacted (for dry-run / stats).
pub fn count_secrets(events: &[SessionEvent]) -> usize {
    let patterns = compiled_patterns();
    let mut count = 0;
    for e in events {
        for p in patterns {
            count += p.regex.find_iter(&e.content).count();
        }
        // Count contextual matches
        count += count_contextual_secrets(&e.content, &e.event_type);
    }
    count
}

// ─── Pattern Definitions ────────────────────────────────────────────────────

struct SecretPattern {
    regex: Regex,
    label: &'static str,
}

fn compiled_patterns() -> &'static Vec<SecretPattern> {
    static PATTERNS: OnceLock<Vec<SecretPattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // ── Layer 1: Vendor-specific prefix patterns ────────────────

            // AWS
            SecretPattern {
                regex: Regex::new(r"(?i)\b((?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z2-7]{16})\b").unwrap(),
                label: "aws_access_key",
            },

            // GitHub
            SecretPattern {
                regex: Regex::new(r"\b(gh[psoa]_[A-Za-z0-9_]{36,255})\b").unwrap(),
                label: "github_token",
            },
            SecretPattern {
                regex: Regex::new(r"\b(github_pat_[A-Za-z0-9_]{22,255})\b").unwrap(),
                label: "github_pat",
            },

            // Anthropic
            SecretPattern {
                regex: Regex::new(r"\b(sk-ant-api\d+-[A-Za-z0-9_\-]{20,})\b").unwrap(),
                label: "anthropic_api_key",
            },

            // OpenAI
            SecretPattern {
                regex: Regex::new(r"\b(sk-[A-Za-z0-9]{20,})\b").unwrap(),
                label: "openai_api_key",
            },

            // Stripe
            SecretPattern {
                regex: Regex::new(r"\b([sr]k_live_[A-Za-z0-9]{20,})\b").unwrap(),
                label: "stripe_key",
            },
            SecretPattern {
                regex: Regex::new(r"\b(rk_live_[A-Za-z0-9]{20,})\b").unwrap(),
                label: "stripe_restricted_key",
            },

            // Slack
            SecretPattern {
                regex: Regex::new(r"\b(xox[bpas]-[A-Za-z0-9\-]{10,})\b").unwrap(),
                label: "slack_token",
            },

            // Google
            SecretPattern {
                regex: Regex::new(r"\b(AIza[A-Za-z0-9_\-]{35})\b").unwrap(),
                label: "google_api_key",
            },

            // Twilio
            SecretPattern {
                regex: Regex::new(r"\b(SK[a-f0-9]{32})\b").unwrap(),
                label: "twilio_api_key",
            },

            // SendGrid
            SecretPattern {
                regex: Regex::new(r"\b(SG\.[A-Za-z0-9_\-]{22}\.[A-Za-z0-9_\-]{43})\b").unwrap(),
                label: "sendgrid_api_key",
            },

            // Mailgun
            SecretPattern {
                regex: Regex::new(r"\b(key-[A-Za-z0-9]{32})\b").unwrap(),
                label: "mailgun_api_key",
            },

            // npm
            SecretPattern {
                regex: Regex::new(r"\b(npm_[A-Za-z0-9]{36})\b").unwrap(),
                label: "npm_token",
            },

            // PyPI
            SecretPattern {
                regex: Regex::new(r"\b(pypi-[A-Za-z0-9_\-]{50,})\b").unwrap(),
                label: "pypi_token",
            },

            // Heroku
            SecretPattern {
                regex: Regex::new(r"(?i)\b(heroku[A-Za-z0-9_\-]*[=:]\s*[A-Fa-f0-9\-]{36})\b").unwrap(),
                label: "heroku_api_key",
            },

            // Cloudflare
            SecretPattern {
                regex: Regex::new(r"\b(v1\.0-[a-f0-9]{24}-[a-f0-9]{146})\b").unwrap(),
                label: "cloudflare_origin_ca",
            },

            // JWT (generic)
            SecretPattern {
                regex: Regex::new(r"\b(eyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]+)\b").unwrap(),
                label: "jwt_token",
            },

            // PEM private keys
            SecretPattern {
                regex: Regex::new(r"(?s)(-----BEGIN[A-Z ]*PRIVATE KEY-----.*?-----END[A-Z ]*PRIVATE KEY-----)").unwrap(),
                label: "private_key",
            },

            // ── Layer 2: Connection strings ─────────────────────────────

            // Database URLs with password
            SecretPattern {
                regex: Regex::new(r"(?i)((?:postgres|postgresql|mysql|mongodb|mongodb\+srv|redis|amqp|mssql)://[^:]+:[^@\s]+@[^\s]+)").unwrap(),
                label: "database_url",
            },

            // ── Layer 3: Generic key=value with sensitive names ──────────

            // Generic: SECRET_KEY=value, API_TOKEN=value, etc.
            SecretPattern {
                regex: Regex::new(r"(?im)^([A-Z_]*(?:SECRET|PASSWORD|PASSWD|TOKEN|API_KEY|APIKEY|ACCESS_KEY|PRIVATE_KEY|AUTH)[A-Z_]*\s*=\s*)(\S+)").unwrap(),
                label: "env_secret",
            },

            // Generic inline: "password": "value", token: 'value', etc.
            SecretPattern {
                regex: Regex::new(r#"(?i)["']?(password|secret|token|api[_\-]?key|access[_\-]?key|private[_\-]?key|auth[_\-]?token|client[_\-]?secret)["']?\s*[:=]\s*["']([^"'\s]{8,})["']"#).unwrap(),
                label: "inline_secret",
            },
        ]
    })
}

// ─── Entropy Detection ──────────────────────────────────────────────────────

/// Shannon entropy of a string (base 2).
fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut freq = [0u32; 256];
    for b in s.bytes() {
        freq[b as usize] += 1;
    }
    let len = s.len() as f64;
    freq.iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// Check if a string looks like a high-entropy secret (random API key).
/// Only triggers for strings that are 16+ chars and look like base64/hex.
fn is_high_entropy_secret(s: &str) -> bool {
    static HEX_RE: OnceLock<Regex> = OnceLock::new();
    static B64_RE: OnceLock<Regex> = OnceLock::new();

    let hex_re = HEX_RE.get_or_init(|| Regex::new(r"^[a-fA-F0-9]{16,}$").unwrap());
    let b64_re = B64_RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9+/=_\-]{16,}$").unwrap());

    if s.len() < 16 {
        return false;
    }

    // Skip UUIDs, git hashes, and common non-secret patterns
    static UUID_RE: OnceLock<Regex> = OnceLock::new();
    let uuid_re = UUID_RE.get_or_init(|| {
        Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").unwrap()
    });
    if uuid_re.is_match(s) {
        return false;
    }

    // Skip file paths
    if s.starts_with('/') || s.starts_with("~/") || s.contains("..") {
        return false;
    }

    if hex_re.is_match(s) {
        return shannon_entropy(s) > 3.0;
    }
    if b64_re.is_match(s) {
        return shannon_entropy(s) > 4.5;
    }

    false
}

// ─── Content Scrubbing ──────────────────────────────────────────────────────

/// Scrub a single content string.
fn scrub_content(content: &str, event_type: &str) -> String {
    let mut result = content.to_string();

    // Layer 1 + 2: Pattern-based replacement
    let patterns = compiled_patterns();
    for p in patterns {
        // Special handling for env_secret: only redact the value part
        if p.label == "env_secret" {
            result = p
                .regex
                .replace_all(&result, |caps: &regex::Captures| {
                    format!("{}[REDACTED:{}]", &caps[1], p.label)
                })
                .to_string();
        } else if p.label == "inline_secret" {
            result = p
                .regex
                .replace_all(&result, |caps: &regex::Captures| {
                    format!(
                        "{}[REDACTED:{}]",
                        &caps[0][..caps[0].len() - caps[2].len()],
                        p.label
                    )
                })
                .to_string();
        } else {
            result = p
                .regex
                .replace_all(&result, format!("[REDACTED:{}]", p.label))
                .to_string();
        }
    }

    // Layer 3: Entropy-based detection in tool_result context
    if event_type == "tool_result" {
        result = scrub_high_entropy_in_tool_output(&result);
    }

    result
}

/// For tool_result events, check each line for key=value patterns where
/// the value has high entropy (likely a randomly generated secret).
fn scrub_high_entropy_in_tool_output(content: &str) -> String {
    static KV_RE: OnceLock<Regex> = OnceLock::new();
    let kv_re =
        KV_RE.get_or_init(|| Regex::new(r"(?m)^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.+)$").unwrap());

    kv_re
        .replace_all(content, |caps: &regex::Captures| {
            let key = &caps[1];
            let value = caps[2].trim();
            if is_high_entropy_secret(value) {
                format!("{}=[REDACTED:high_entropy]", key)
            } else {
                caps[0].to_string()
            }
        })
        .to_string()
}

/// Count contextual secrets (for dry-run reporting).
fn count_contextual_secrets(content: &str, event_type: &str) -> usize {
    if event_type != "tool_result" {
        return 0;
    }
    static KV_RE: OnceLock<Regex> = OnceLock::new();
    let kv_re =
        KV_RE.get_or_init(|| Regex::new(r"(?m)^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.+)$").unwrap());
    kv_re
        .captures_iter(content)
        .filter(|caps| {
            let value = caps[2].trim();
            is_high_entropy_secret(value)
        })
        .count()
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aws_key() {
        let input = "Found key AKIAIOSFODNN7EXAMPLE in config";
        let result = scrub_content(input, "user");
        assert!(result.contains("[REDACTED:aws_access_key]"));
        assert!(!result.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn test_github_token() {
        let input = "token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij";
        let result = scrub_content(input, "user");
        assert!(result.contains("[REDACTED:github_token]"));
    }

    #[test]
    fn test_anthropic_key() {
        let input = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz1234567890";
        let result = scrub_content(input, "user");
        assert!(result.contains("[REDACTED:anthropic_api_key]"));
    }

    #[test]
    fn test_jwt() {
        let input = "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc123xyz";
        let result = scrub_content(input, "user");
        assert!(result.contains("[REDACTED:jwt_token]"));
    }

    #[test]
    fn test_database_url() {
        let input = "DATABASE_URL=postgres://admin:s3cretP@ss@prod-db:5432/myapp";
        let result = scrub_content(input, "tool_result");
        assert!(result.contains("[REDACTED:"));
        assert!(!result.contains("s3cretP@ss"));
    }

    #[test]
    fn test_pem_key() {
        let input =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA...\n-----END RSA PRIVATE KEY-----";
        let result = scrub_content(input, "tool_result");
        assert!(result.contains("[REDACTED:private_key]"));
    }

    #[test]
    fn test_env_secret() {
        let input = "SECRET_KEY=mysupersecretvalue123\nDEBUG=true\nAPI_TOKEN=abc123def456";
        let result = scrub_content(input, "tool_result");
        assert!(result.contains("SECRET_KEY=[REDACTED:env_secret]"));
        assert!(result.contains("API_TOKEN=[REDACTED:env_secret]"));
        assert!(result.contains("DEBUG=true")); // not redacted
    }

    #[test]
    fn test_stripe_key() {
        // Use a clearly fake key that won't trigger GitHub push protection
        let fake_key = format!("sk_live_{}", "X".repeat(24));
        let input = format!("Using key {}", fake_key);
        let result = scrub_content(&input, "user");
        assert!(result.contains("[REDACTED:stripe_key]"));
        assert!(!result.contains(&fake_key));
    }

    #[test]
    fn test_slack_token() {
        // Use a clearly fake token
        let fake_token = format!("xoxb-0000000000-{}", "X".repeat(24));
        let input = format!("SLACK_BOT_TOKEN={}", fake_token);
        let result = scrub_content(&input, "tool_result");
        assert!(result.contains("[REDACTED:"));
    }

    #[test]
    fn test_high_entropy_value() {
        let input = "MY_VAR=aB3xK9mP2wQ7rT5uY8vZ1nL4oJ6iH0g";
        let result = scrub_content(input, "tool_result");
        // Should be caught by entropy detection
        assert!(result.contains("[REDACTED:"));
    }

    #[test]
    fn test_normal_text_not_redacted() {
        let input = "Hello world, this is a normal conversation about coding.";
        let result = scrub_content(input, "user");
        assert_eq!(result, input);
    }

    #[test]
    fn test_uuid_not_redacted() {
        let input = "session_id=a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let result = scrub_content(input, "tool_result");
        assert!(result.contains("a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
    }

    #[test]
    fn test_scrub_events() {
        let fake_stripe = format!("sk_live_{}", "X".repeat(24));
        let events = vec![
            SessionEvent {
                timestamp: 1000,
                event_type: "user".to_string(),
                tool: None,
                content: format!("set my key to {}", fake_stripe),
                ..Default::default()
            },
            SessionEvent {
                timestamp: 2000,
                event_type: "tool_result".to_string(),
                tool: Some("Bash".to_string()),
                content: "SECRET_KEY=hunter2\nDEBUG=true".to_string(),
                ..Default::default()
            },
        ];
        let scrubbed = scrub_events(&events);
        assert_eq!(scrubbed.len(), 2);
        assert!(scrubbed[0].content.contains("[REDACTED:stripe_key]"));
        assert!(scrubbed[1].content.contains("[REDACTED:env_secret]"));
        assert!(scrubbed[1].content.contains("DEBUG=true"));
    }

    #[test]
    fn test_shannon_entropy() {
        // Low entropy (all same char)
        assert!(shannon_entropy("aaaaaaaaaa") < 1.0);
        // High entropy (random-looking)
        assert!(shannon_entropy("aB3xK9mP2wQ7rT5u") > 3.5);
    }

    #[test]
    fn test_inline_secret() {
        let input = r#"{"password": "my-super-secret-pass123"}"#;
        let result = scrub_content(input, "assistant");
        assert!(result.contains("[REDACTED:inline_secret]"));
        assert!(!result.contains("my-super-secret-pass123"));
    }

    #[test]
    fn test_connection_string() {
        let input = "mongodb+srv://admin:p4ssw0rd@cluster0.abc.mongodb.net/mydb";
        let result = scrub_content(input, "tool_result");
        assert!(result.contains("[REDACTED:database_url]"));
        assert!(!result.contains("p4ssw0rd"));
    }
}
