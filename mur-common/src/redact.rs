//! One redaction chokepoint, shared by every writer that puts text on disk.
//!
//! This lived in `mur-agent-runtime::hooks::b0_helpers` and was reachable only
//! from the runtime's own telemetry writer. B0 rule 9 is named "telemetry sink
//! redaction", which reads like a guarantee about everything MUR writes — it
//! was not. The CLI hook pipeline's capture queue
//! (`mur-core::inject::queue`) went to disk unredacted, and on a real install
//! accumulated 934 MB of verbatim command lines including API keys (#979).
//!
//! It sits in `mur-common` because both `mur-agent-runtime` and `mur-core`
//! write text, and neither may depend on the other for it.
//!
//! `mur-common::skill::scan::secrets` DETECTS secrets and reports findings;
//! this module REPLACES them. The two are deliberately separate: a scanner
//! that silently rewrote its input would be a surprising scanner.

pub fn redact_secrets(input: &str) -> std::borrow::Cow<'_, str> {
    // TODO(M1): collapse into mur-common::skill::scan::secrets
    use regex_lite::Regex;
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
    use regex_lite::Regex;
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

/// Redact every string leaf of a JSON value in place.
///
/// The shape the telemetry writer already used, moved here so both writers
/// share it. Walking the tree rather than regexing the serialised line is the
/// safe form: a replacement lands inside a JSON string and cannot break the
/// structure around it.
pub fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => {
            let stage1 = redact_secrets(s);
            let stage2 = redact_home_path(&stage1);
            // Only allocate-and-replace if something actually changed.
            if stage2 != *s {
                *s = stage2.into_owned();
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_value(item);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values_mut() {
                redact_value(v);
            }
        }
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) | serde_json::Value::Null => {}
    }
}
