//! Runtime additions to the egress proxy's policy, and the `egress/denied`
//! signal (#1907, spec Part 3).
//!
//! The proxy starts before the sandbox seals, long before the socket notifier
//! exists, so the event sink is attached later ([`EgressProxyHandle::attach_events`]).
//! Until then denials are only logged — the headless path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mur_common::egress_event::{DENIED_EVENT_WINDOW_SECS, DENIED_METHOD, DenyReason, EgressDenied};
use tokio::sync::mpsc;

use super::{EgressProxyHandle, Registry};

/// Where denials go once a client-facing notifier exists.
#[derive(Clone, Default)]
pub(super) struct Events {
    inner: Arc<Mutex<EventsInner>>,
}

#[derive(Default)]
struct EventsInner {
    agent: String,
    sink: Option<mpsc::Sender<serde_json::Value>>,
    last: HashMap<(String, String), Instant>,
}

impl Events {
    pub(super) fn new(agent: &str) -> Self {
        let e = Self::default();
        e.inner.lock().unwrap().agent = agent.to_owned();
        e
    }

    /// Emit at most one event per `(server, host)` per window. Returns the
    /// event when one was sent, for tests and the log line.
    pub(super) fn denied(
        &self,
        server: &str,
        host: &str,
        port: Option<u16>,
        broad: bool,
        now: Instant,
    ) -> Option<EgressDenied> {
        let mut g = self.inner.lock().unwrap();
        let key = (server.to_owned(), host.to_owned());
        let window = Duration::from_secs(DENIED_EVENT_WINDOW_SECS);
        if g.last
            .get(&key)
            .is_some_and(|t| now.duration_since(*t) < window)
        {
            return None;
        }
        g.last.insert(key, now);
        let ev = EgressDenied {
            agent: g.agent.clone(),
            server: server.to_owned(),
            host: host.to_owned(),
            port,
            mode: if broad { "broad_audited" } else { "restricted" }.to_owned(),
            reason: if broad {
                DenyReason::DenyListed
            } else {
                DenyReason::NotAllowed
            },
            at: chrono::Utc::now().to_rfc3339(),
        };
        if let Some(sink) = &g.sink {
            let n = serde_json::json!({
                "jsonrpc": "2.0",
                "method": DENIED_METHOD,
                "params": ev,
            });
            // Never block the proxy on a slow client: drop when full.
            let _ = sink.try_send(n);
        }
        Some(ev)
    }
}

/// Why a runtime grant was refused. Text is user-facing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    /// No running server by that name is behind the proxy.
    UnknownServer(String),
    /// The server is `BroadAudited`: every host but the deny list is already
    /// reachable, and a deny-listed host is never grantable (D3).
    DenyListed { server: String, host: String },
    /// The host failed the shared validator.
    InvalidHost(String),
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownServer(s) => write!(
                f,
                "no running MCP server `{s}` is routed through this agent's egress proxy"
            ),
            Self::DenyListed { server, host } => write!(
                f,
                "`{host}` is on `{server}`'s deny list; a deny-listed host cannot be granted. \
                 Remove the entry with `mur agent mcp set-network` (previewed and audited)"
            ),
            Self::InvalidHost(e) => write!(f, "{e}"),
        }
    }
}

impl EgressProxyHandle {
    /// Give the proxy somewhere to send `egress/denied`. Called once the
    /// supervisor's notifier exists.
    pub fn attach_events(&self, sink: mpsc::Sender<serde_json::Value>) {
        self.events.inner.lock().unwrap().sink = Some(sink);
    }

    /// Let `host` through for `server` from the next connection on. Applies to
    /// every token registered for that server (a respawned child gets a new
    /// one). Returns the normalized host that was added.
    pub fn grant(&self, server: &str, host: &str) -> Result<String, GrantError> {
        let n = mur_common::host_pattern::normalize_host_pattern(host)
            .map_err(|e| GrantError::InvalidHost(format!("`{host}` is not a host: {e}")))?;
        grant_in(&self.registry, server, &n.value)?;
        Ok(n.value)
    }
}

fn grant_in(registry: &Registry, server: &str, host: &str) -> Result<(), GrantError> {
    let mut reg = registry.lock().unwrap();
    let mut found = false;
    for e in reg.values_mut().filter(|e| e.server == server) {
        found = true;
        if e.broad {
            return Err(GrantError::DenyListed {
                server: server.to_owned(),
                host: host.to_owned(),
            });
        }
        if !e.allow.iter().any(|h| h == host) {
            e.allow.push(host.to_owned());
        }
    }
    if found {
        Ok(())
    } else {
        Err(GrantError::UnknownServer(server.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_event_per_server_host_per_window() {
        let ev = Events::new("mur");
        let (tx, mut rx) = mpsc::channel(8);
        ev.inner.lock().unwrap().sink = Some(tx);
        let t0 = Instant::now();
        let first = ev
            .denied("browser", "img.x.com", Some(443), false, t0)
            .unwrap();
        assert_eq!(first.reason, DenyReason::NotAllowed);
        assert_eq!(first.agent, "mur");
        assert!(
            ev.denied(
                "browser",
                "img.x.com",
                Some(443),
                false,
                t0 + Duration::from_secs(59)
            )
            .is_none()
        );
        // Different host or server: its own window.
        assert!(ev.denied("browser", "cdn.x.com", None, false, t0).is_some());
        assert!(ev.denied("search", "img.x.com", None, true, t0).is_some());
        assert!(
            ev.denied(
                "browser",
                "img.x.com",
                None,
                false,
                t0 + Duration::from_secs(DENIED_EVENT_WINDOW_SECS)
            )
            .is_some()
        );
        let n = rx.try_recv().unwrap();
        assert_eq!(n["method"], DENIED_METHOD);
        assert_eq!(n["params"]["host"], "img.x.com");
    }

    #[test]
    fn deny_list_hits_are_tagged_ungrantable() {
        let ev = Events::new("mur");
        let d = ev
            .denied("search", "169.254.169.254", None, true, Instant::now())
            .unwrap();
        assert_eq!(d.reason, DenyReason::DenyListed);
        assert!(!d.grantable());
    }

    #[test]
    fn grant_adds_to_every_token_of_that_server_only() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        let a = h.register_server("browser", vec!["shop.example.com".into()], vec![], false);
        let b = h.register_server("browser", vec![], vec![], false);
        let other = h.register_server("fetch", vec![], vec![], false);
        assert_eq!(
            h.grant("browser", "IMG.Shop.Example.com").unwrap(),
            "img.shop.example.com"
        );
        let reg = h.registry.lock().unwrap();
        assert!(reg[&a].allow.contains(&"img.shop.example.com".to_owned()));
        assert!(reg[&b].allow.contains(&"img.shop.example.com".to_owned()));
        assert!(reg[&other].allow.is_empty());
    }

    #[test]
    fn grant_refuses_broad_unknown_and_garbage() {
        let h = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        h.register_server("search", vec![], vec!["evil.example".into()], true);
        assert!(matches!(
            h.grant("search", "evil.example"),
            Err(GrantError::DenyListed { .. })
        ));
        assert!(matches!(
            h.grant("nope", "a.com"),
            Err(GrantError::UnknownServer(_))
        ));
        assert!(matches!(
            h.grant("search", "評分，"),
            Err(GrantError::InvalidHost(_))
        ));
    }
}
