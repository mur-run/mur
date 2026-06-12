//! Decide whether a Hub-spawned agent runtime should route its Anthropic
//! traffic through the local OAuth bridge (cc-proxy).
//!
//! Subscription tokens (`sk-ant-oat*`) are rejected by api.anthropic.com when
//! sent as `x-api-key`; they must traverse cc-proxy, which swaps the header for
//! the Bearer + claude-code betas disguise. The Hub therefore injects
//! `ANTHROPIC_BASE_URL` pointing at the bridge when it spawns a runtime — but
//! only when the bridge is configured-on and actually listening, so a stopped
//! cc-proxy degrades to the direct path instead of failing every request with a
//! confusing 401.

use mur_common::config::CcProxyConfig;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// How long to wait for the bridge TCP handshake before deciding it's down.
/// The bridge is loopback, so a live one answers in well under a millisecond;
/// this only bounds the wait when nothing is listening.
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Resolve the `ANTHROPIC_BASE_URL` to inject into a spawned runtime, or `None`
/// to leave it on its inherited/default (direct) path.
///
/// `env_override` is the Hub process's own `ANTHROPIC_BASE_URL` (if any): when
/// the operator has set one explicitly we respect it and inject nothing — the
/// runtime inherits it. `probe` reports whether the bridge is reachable, taken
/// as a parameter so the decision is unit-testable without a live socket.
pub fn resolve_bridge_url(
    cfg: &CcProxyConfig,
    env_override: Option<&str>,
    probe: impl Fn(&str) -> bool,
) -> Option<String> {
    // An explicit ANTHROPIC_BASE_URL on the Hub wins — don't second-guess it.
    if env_override.is_some_and(|v| !v.trim().is_empty()) {
        return None;
    }
    if !cfg.enabled {
        return None;
    }
    if !probe(&cfg.url) {
        return None; // bridge down → fall back to the direct path
    }
    Some(cfg.url.clone())
}

/// Real reachability probe: a short-timeout TCP connect to the bridge's
/// host:port. Any parse or connect failure reads as "not listening".
pub fn bridge_is_listening(url: &str) -> bool {
    let Some(authority) = authority(url) else {
        return false;
    };
    match authority.to_socket_addrs() {
        Ok(addrs) => {
            let mut addrs = addrs;
            addrs.any(|addr| TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok())
        }
        Err(_) => false,
    }
}

/// Extract `host:port` from a URL, defaulting the port from the scheme when
/// absent. Returns `None` for anything we can't confidently turn into an
/// address (no scheme, empty host, unknown scheme without an explicit port).
fn authority(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host_port = rest.split(['/', '?', '#']).next()?.trim();
    if host_port.is_empty() {
        return None;
    }
    if host_port.contains(':') {
        // Already has an explicit port (covers `host:port` and `[::1]:port`).
        Some(host_port.to_string())
    } else {
        let port = match scheme {
            "https" => 443,
            "http" => 80,
            _ => return None,
        };
        Some(format!("{host_port}:{port}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(url: &str, enabled: bool) -> CcProxyConfig {
        CcProxyConfig {
            url: url.to_string(),
            enabled,
        }
    }

    #[test]
    fn injects_when_enabled_and_listening() {
        let c = cfg("http://127.0.0.1:8088", true);
        assert_eq!(
            resolve_bridge_url(&c, None, |_| true),
            Some("http://127.0.0.1:8088".to_string())
        );
    }

    #[test]
    fn none_when_disabled() {
        let c = cfg("http://127.0.0.1:8088", false);
        assert_eq!(resolve_bridge_url(&c, None, |_| true), None);
    }

    #[test]
    fn none_when_bridge_down() {
        let c = cfg("http://127.0.0.1:8088", true);
        assert_eq!(resolve_bridge_url(&c, None, |_| false), None);
    }

    #[test]
    fn explicit_env_override_wins() {
        let c = cfg("http://127.0.0.1:8088", true);
        // Even with the bridge up, an operator-set base URL is left untouched.
        assert_eq!(
            resolve_bridge_url(&c, Some("https://corp-egress.local"), |_| true),
            None
        );
    }

    #[test]
    fn blank_env_override_is_ignored() {
        let c = cfg("http://127.0.0.1:8088", true);
        assert_eq!(
            resolve_bridge_url(&c, Some("   "), |_| true),
            Some("http://127.0.0.1:8088".to_string())
        );
    }

    #[test]
    fn authority_keeps_explicit_port() {
        assert_eq!(
            authority("http://127.0.0.1:8088").as_deref(),
            Some("127.0.0.1:8088")
        );
    }

    #[test]
    fn authority_defaults_scheme_port() {
        assert_eq!(
            authority("https://api.anthropic.com").as_deref(),
            Some("api.anthropic.com:443")
        );
        assert_eq!(
            authority("http://localhost").as_deref(),
            Some("localhost:80")
        );
    }

    #[test]
    fn authority_strips_path() {
        assert_eq!(
            authority("http://127.0.0.1:8088/v1/messages").as_deref(),
            Some("127.0.0.1:8088")
        );
    }

    #[test]
    fn authority_rejects_garbage() {
        assert_eq!(authority("not-a-url"), None);
        assert_eq!(authority("ftp://host"), None); // unknown scheme, no explicit port
        assert_eq!(authority("http://"), None);
    }

    #[test]
    fn down_bridge_probe_is_false() {
        // Port 1 on loopback is not listening in any sane test env.
        assert!(!bridge_is_listening("http://127.0.0.1:1"));
        assert!(!bridge_is_listening("garbage"));
    }
}
