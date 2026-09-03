//! The one URL shape a subscription provider may dial: this machine's
//! gateway, over plain HTTP, on an explicit port, at exactly the route that
//! provider is for. Shared by `codex` (`/codex/v1`) and `claude` (`/v1`).
//! The loopback restriction is a safety property — an authless request to a
//! remote host is either a request to a stranger or a route that lands on
//! metered billing — so it is enforced here, not merely validated in the Hub.

use super::LlmError;

pub fn validate_loopback_base_url(
    raw: &str,
    required_path: &str,
) -> Result<reqwest::Url, LlmError> {
    let bad = |why: &str| LlmError::Http(format!("base_url {raw:?} rejected: {why}"));
    let url = reqwest::Url::parse(raw).map_err(|e| bad(&e.to_string()))?;
    if url.scheme() != "http" {
        return Err(bad("scheme must be http (loopback only)"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("credentials in the URL are not allowed"));
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if !loopback {
        return Err(bad("host must be localhost or a loopback IP"));
    }
    if url.port().is_none() {
        return Err(bad("an explicit port is required"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(bad("query and fragment are not allowed"));
    }
    if url.path().trim_end_matches('/') != required_path {
        return Err(bad(&format!("path must be exactly {required_path}")));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_required_path_is_the_only_thing_that_differs_between_providers() {
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1", "/v1").is_ok());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1/", "/v1").is_ok());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/codex/v1", "/v1").is_err());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1", "/codex/v1").is_err());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1/messages", "/v1").is_err());
    }
}
