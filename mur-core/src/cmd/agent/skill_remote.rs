//! Remote skill install — validate URL, fetch, preview (parse+scan), install.

use anyhow::{Result, bail};
use reqwest::Url;

/// Validate and normalize a remote skill URL. Requires `https`; `http` is
/// allowed only for localhost development.
pub fn validate_skill_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
    let host = url.host_str().unwrap_or("");
    let is_local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    match url.scheme() {
        "https" => {}
        "http" if is_local => {}
        "http" => bail!("skill URLs require https (got http://{host})"),
        other => bail!("unsupported URL scheme: {other}"),
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation() {
        assert!(validate_skill_url("https://example.com/skill.yaml").is_ok());
        assert!(validate_skill_url("http://localhost:8080/skill.md").is_ok());
        assert!(validate_skill_url("http://127.0.0.1/skill.md").is_ok());
        assert!(validate_skill_url("http://example.com/skill.yaml").is_err());
        assert!(validate_skill_url("ftp://example.com/skill.yaml").is_err());
        assert!(validate_skill_url("not a url").is_err());
    }
}
