//! Token lifetime: a registered proxy token belongs to one MCP client
//! generation and is revoked when that generation ends.
//!
//! Before hot reload a token lived as long as the agent, so it was never
//! removed. Now an edited entry starts a new client generation with a fresh
//! token and new hosts; the old token must stop working once the last call on
//! the old generation is done, or a retired child that is still running
//! would keep the old allowlist forever.

use super::{EgressProxyHandle, Registry};

impl EgressProxyHandle {
    /// Revoke `token`: later requests carrying it get the same 407 as an
    /// unknown token. Returns whether it was registered.
    pub fn unregister(&self, token: &str) -> bool {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token)
            .is_some()
    }

    /// How many live tokens `server` holds: one per client generation still
    /// referenced. A retired generation's token is gone once its last call
    /// finishes.
    pub fn tokens_for(&self, server: &str) -> usize {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|e| e.server == server)
            .count()
    }

    /// [`Self::register_server`], held by a [`TokenLease`] that revokes the
    /// token when dropped.
    pub fn lease_server(
        &self,
        server: &str,
        allow_hosts: Vec<String>,
        deny_hosts: Vec<String>,
        broad: bool,
    ) -> TokenLease {
        let token = self.register_server(server, allow_hosts, deny_hosts, broad);
        TokenLease {
            token,
            registry: self.registry.clone(),
        }
    }
}

/// A registered proxy token, revoked on drop. Owned by the MCP client whose
/// child was handed it, so it lives exactly as long as that client generation.
pub struct TokenLease {
    token: String,
    registry: Registry,
}

impl TokenLease {
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl std::fmt::Debug for TokenLease {
    // The token is a bearer credential; never print it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenLease(..)")
    }
}

impl Drop for TokenLease {
    fn drop(&mut self) {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> EgressProxyHandle {
        EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap())
    }

    #[test]
    fn unregister_removes_only_that_token() {
        let h = handle();
        let a = h.register_server("browser", vec!["a.example".into()], vec![], false);
        let b = h.register_server("browser", vec!["b.example".into()], vec![], false);
        assert!(h.unregister(&a));
        assert!(!h.unregister(&a), "second revoke finds nothing");
        let reg = h.registry.lock().unwrap();
        assert!(!reg.contains_key(&a));
        assert!(reg.contains_key(&b));
    }

    #[test]
    fn a_lease_keeps_its_token_until_dropped() {
        let h = handle();
        let lease = h.lease_server("browser", vec!["a.example".into()], vec![], false);
        let token = lease.token().to_owned();
        assert!(h.registry.lock().unwrap().contains_key(&token));
        drop(lease);
        assert!(!h.registry.lock().unwrap().contains_key(&token));
    }

    #[test]
    fn a_lease_never_prints_its_token() {
        let lease = handle().lease_server("browser", vec![], vec![], false);
        assert!(!format!("{lease:?}").contains(lease.token()));
    }
}
