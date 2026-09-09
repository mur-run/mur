//! The one URL shape a subscription provider may dial: this machine's
//! gateway, over plain HTTP, on an explicit port, at exactly the route that
//! provider is for. Shared by `codex` (`/codex/v1`) and `claude` (`/v1`).
//! The loopback restriction is a safety property — an authless request to a
//! remote host is either a request to a stranger or a route that lands on
//! metered billing — so it is enforced here, not merely validated in the Hub.

use super::LlmError;

/// True when `url`'s host is this machine. Split out of
/// [`validate_loopback_base_url`] because two callers need the host test
/// without the rest: that validator also pins scheme, port and an exact route
/// path, which is right for a subscription gateway and wrong for an arbitrary
/// local inference server.
fn host_is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// True when `raw` points at an OpenAI-compatible server on this machine.
/// Unlike [`validate_loopback_base_url`] this asks only "is this host me?" —
/// a local runtime picks its own port and route, so pinning either would just
/// reject working endpoints.
pub fn is_loopback_base_url(raw: &str) -> bool {
    reqwest::Url::parse(raw).is_ok_and(|u| host_is_loopback(&u))
}

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
    if !host_is_loopback(&url) {
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

    /// A local inference server (oMLX, LM Studio, vLLM) is reached at
    /// whatever port and path it chose, so only the host may be judged here.
    #[test]
    fn any_port_and_path_on_this_machine_counts_as_local() {
        for ok in [
            "http://127.0.0.1:8000/v1",
            "http://localhost:1234/v1",
            "http://127.0.0.1:11434/v1/",
            "https://127.0.0.1:8000/v1",
            "http://[::1]:8000/v1",
        ] {
            assert!(is_loopback_base_url(ok), "{ok}");
        }
    }

    /// The whole point of the check: a remote host must not be treated as a
    /// keyless local server. `localhost.evil.test` is the near-miss that a
    /// `starts_with`/`contains` test would wave through.
    #[test]
    fn a_remote_host_is_never_local_however_it_is_spelled() {
        for bad in [
            "https://api.openai.com/v1",
            "http://localhost.evil.test:8000/v1",
            "http://192.168.1.10:8000/v1",
            "not a url",
        ] {
            assert!(!is_loopback_base_url(bad), "{bad}");
        }
    }

    #[test]
    fn the_required_path_is_the_only_thing_that_differs_between_providers() {
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1", "/v1").is_ok());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1/", "/v1").is_ok());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/codex/v1", "/v1").is_err());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1", "/codex/v1").is_err());
        assert!(validate_loopback_base_url("http://127.0.0.1:8088/v1/messages", "/v1").is_err());
    }
}
