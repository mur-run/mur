use regex_lite::Regex;
use std::sync::OnceLock;

fn patterns() -> &'static [(Regex, &'static str)] {
    static P: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    P.get_or_init(|| {
        vec![
            (Regex::new(r"\bsk-[a-zA-Z0-9]{20,}\b").unwrap(), "openai_key"),
            (Regex::new(r"\bsk-ant-[a-zA-Z0-9-]{20,}\b").unwrap(), "anthropic_key"),
            (Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(), "aws_access_key"),
            (
                Regex::new(r"\baws_secret_access_key\s*[:=]\s*[A-Za-z0-9/+=]{40}\b").unwrap(),
                "aws_secret_key",
            ),
            (Regex::new(r"\bghp_[A-Za-z0-9]{36}\b").unwrap(), "github_pat"),
            (Regex::new(r"\bghs_[A-Za-z0-9]{36}\b").unwrap(), "github_app_token"),
            (Regex::new(r"\bAIza[0-9A-Za-z_-]{35}\b").unwrap(), "gcp_api_key"),
            (
                Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\b").unwrap(),
                "jwt",
            ),
            (
                Regex::new(r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----").unwrap(),
                "pem_private_key",
            ),
            (
                Regex::new(r"\bhooks\.slack\.com/services/T[A-Z0-9]+/B[A-Z0-9]+/[A-Za-z0-9]+\b").unwrap(),
                "slack_webhook",
            ),
            (
                Regex::new(
                    r"(?i)\b(api_key|api_secret|secret_key|access_token|password|token)\s*[:=]\s*[A-Za-z0-9_\-./+=]{20,}\b",
                )
                .unwrap(),
                "env_assignment",
            ),
        ]
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct SecretFinding {
    pub label: &'static str,
    pub matched: String,
}

pub fn scan_secrets(body: &str) -> Vec<SecretFinding> {
    let mut out = Vec::new();
    for (rx, label) in patterns() {
        for m in rx.find_iter(body) {
            out.push(SecretFinding { label, matched: m.as_str().to_string() });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_openai_key() {
        let f = scan_secrets("here is my key: sk-abcd1234567890efghij1234");
        assert!(f.iter().any(|x| x.label == "openai_key"));
    }

    #[test]
    fn detects_anthropic_key() {
        let f = scan_secrets("sk-ant-abcdefghijklmnopqrst-1234");
        assert!(f.iter().any(|x| x.label == "anthropic_key"));
    }

    #[test]
    fn detects_aws_access_key() {
        let f = scan_secrets("AKIAIOSFODNN7EXAMPLE");
        assert!(f.iter().any(|x| x.label == "aws_access_key"));
    }

    #[test]
    fn detects_github_pat() {
        let f = scan_secrets("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(f.iter().any(|x| x.label == "github_pat"));
    }

    #[test]
    fn detects_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.SflKxwRJSMeKKF2QT4fwpMeJf36";
        let f = scan_secrets(jwt);
        assert!(f.iter().any(|x| x.label == "jwt"));
    }

    #[test]
    fn clean_body_returns_empty() {
        assert!(scan_secrets("nothing to see").is_empty());
    }
}
