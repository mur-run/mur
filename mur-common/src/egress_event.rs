//! Wire types for just-in-time egress grants (#1907).
//!
//! The runtime's egress proxy broadcasts [`EgressDenied`] when a scoped MCP
//! server is refused a host. The event is data only — no rendering hints — so
//! any client on the agent socket (murmur TUI, plain mode, Commander) can
//! render it its own way (spec D2). [`GRANT_METHOD`] is how a client tells the
//! running proxy to let a host through without a restart.

use serde::{Deserialize, Serialize};

/// JSON-RPC notification method for a denied egress connection.
pub const DENIED_METHOD: &str = "egress/denied";

/// A2A method that adds a host to a running proxy's policy.
pub const GRANT_METHOD: &str = "egress/grant";

/// At most one [`EgressDenied`] per `(server, host, audience)` in this window,
/// where the audience is the turn the denial is routed to, or everyone when
/// it is unattributed.
pub const DENIED_EVENT_WINDOW_SECS: u64 = 60;

/// Why the proxy refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// `Restricted`: the host is not on the allow-list. Grantable.
    NotAllowed,
    /// `BroadAudited`: the host is on the deny list. Never grantable (D3).
    DenyListed,
}

/// `params` of a [`DENIED_METHOD`] notification. The denied connection has
/// already failed; a grant affects only later connections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressDenied {
    pub agent: String,
    pub server: String,
    pub host: String,
    pub port: Option<u16>,
    /// The server's network mode, as stored (`restricted` / `broad_audited`).
    pub mode: String,
    pub reason: DenyReason,
    /// RFC 3339 timestamp.
    pub at: String,
    /// The turn whose tool call was using `server` when the denial happened,
    /// when exactly one was. Lets the runtime route the notice to that turn's
    /// connection instead of every connection. `None` when no turn (a child
    /// started outside a tool call) or more than one turn was using the
    /// server — the runtime then falls back to the broadcast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl EgressDenied {
    /// Whether a client may offer a grant for this denial (D3).
    pub fn grantable(&self) -> bool {
        self.reason == DenyReason::NotAllowed
    }
}

/// `params` of [`GRANT_METHOD`]. `session: true` lives in memory until the
/// agent restarts; `false` mirrors a grant the caller already wrote to the
/// profile, so it survives restart because the profile does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressGrant {
    pub server: String,
    pub host: String,
    #[serde(default)]
    pub session: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_round_trips_and_only_allow_list_misses_are_grantable() {
        let d = EgressDenied {
            agent: "mur".into(),
            server: "browser".into(),
            host: "img.shop.example.com".into(),
            port: Some(443),
            mode: "restricted".into(),
            reason: DenyReason::NotAllowed,
            at: "2026-10-15T00:00:00Z".into(),
            task_id: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["reason"], "not_allowed");
        // A client older than this field must still parse the frame.
        assert!(v.get("task_id").is_none());
        assert_eq!(serde_json::from_value::<EgressDenied>(v).unwrap(), d);
        assert!(d.grantable());
        let listed = EgressDenied {
            reason: DenyReason::DenyListed,
            ..d
        };
        assert!(!listed.grantable());
    }
}
