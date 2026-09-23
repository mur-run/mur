//! Domain allowlist for authenticated browser profiles.
//!
//! A profile carries a logged-in session. Without a guard, anything replayed
//! under that profile could navigate anywhere the account can reach. The
//! allowlist pins navigation to the domains the profile was created for.
//!
//! Matching rules:
//! - only absolute `http`/`https` URLs with a host are ever allowed;
//! - an empty allowlist means "no domain restriction" (backwards compatible
//!   with profiles created before allowlists existed);
//! - an entry matches its exact host or any subdomain of it, so `example.com`
//!   allows `app.example.com` but never `notexample.com` or
//!   `example.com.evil.com`;
//! - hosts and entries compare case-insensitively, ignoring a trailing `.`.

use anyhow::{Result, bail};

/// True when `url` may be visited under a profile whose allowlist is `allow`.
pub fn is_allowed(url: &str, allow: &[String]) -> bool {
    let Some(host) = http_host(url) else {
        return false;
    };
    allow.is_empty() || allow.iter().any(|entry| host_matches(&host, entry))
}

/// Like [`is_allowed`], but returns an error naming the rejected host.
pub fn check(url: &str, allow: &[String]) -> Result<()> {
    let Some(host) = http_host(url) else {
        bail!("navigation to {url:?} blocked: only absolute http(s) URLs are allowed");
    };
    if allow.is_empty() || allow.iter().any(|entry| host_matches(&host, entry)) {
        return Ok(());
    }
    bail!(
        "navigation to host {host:?} blocked: not in profile allow_domains [{}]",
        allow.join(", ")
    )
}

/// Normalised host of an absolute http(s) URL, or `None` for anything else.
fn http_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.host_str().map(normalise).filter(|h| !h.is_empty())
}

fn host_matches(host: &str, entry: &str) -> bool {
    let entry = normalise(entry);
    if entry.is_empty() {
        return false;
    }
    host == entry
        || host
            .strip_suffix(entry.as_str())
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn normalise(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn empty_allowlist_allows_any_http_url() {
        assert!(is_allowed("https://evil.com/x", &[]));
        assert!(is_allowed("http://localhost:3000/", &[]));
    }

    #[test]
    fn exact_host_and_subdomains_are_allowed() {
        let allow = list(&["example.com"]);
        assert!(is_allowed("https://example.com/a", &allow));
        assert!(is_allowed("https://app.example.com/b", &allow));
        assert!(is_allowed("https://a.b.example.com:8443/c?d=e", &allow));
    }

    #[test]
    fn lookalike_hosts_are_rejected() {
        let allow = list(&["example.com"]);
        assert!(!is_allowed("https://evil.com", &allow));
        assert!(!is_allowed("https://notexample.com", &allow));
        assert!(!is_allowed("https://example.com.evil.com/", &allow));
        // userinfo trick: the real host is evil.com
        assert!(!is_allowed("https://example.com@evil.com/", &allow));
    }

    #[test]
    fn non_http_schemes_are_rejected_even_without_allowlist() {
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "ftp://example.com/",
            "about:blank",
        ] {
            assert!(!is_allowed(url, &[]), "{url} should be rejected");
            assert!(!is_allowed(url, &list(&["example.com"])), "{url}");
        }
    }

    #[test]
    fn unparseable_or_relative_urls_are_rejected() {
        assert!(!is_allowed("not a url", &[]));
        assert!(!is_allowed("/relative/path", &list(&["example.com"])));
    }

    #[test]
    fn matching_ignores_case_and_trailing_dot() {
        let allow = list(&["Example.COM."]);
        assert!(is_allowed("https://APP.example.com/", &allow));
        assert!(is_allowed("https://example.com./", &allow));
    }

    #[test]
    fn check_error_names_the_host() {
        let err = check("https://evil.com/steal", &list(&["example.com"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("evil.com"), "{err}");
        assert!(check("https://example.com/", &list(&["example.com"])).is_ok());
    }
}
