//! Owner-only socket methods, and the proof that a call comes from the owner.
//!
//! An agent's socket is reachable by every peer its `accepts_from` admits —
//! with the usual `['*']`, every other agent. Some methods there are the
//! owner's controls, not conversation: rewriting the vault, switching the
//! model, cancelling or steering a turn, forgetting HITL decisions. A peer
//! that may *talk* to an agent must not *operate* it.
//!
//! The runtime admits an owner method without proof only from stdio, whose
//! peer launched it. Every socket caller, the agent's own lineage included,
//! must present a proof derived from the home's approval token
//! ([`crate::hitl::approval_token`]): it lives in `secrets/`, which sealed
//! agents can neither read nor write, so the Hub and the CLI can prove they
//! are the user and an agent's `bash` cannot. The one exception is a task's
//! submitter, which may `tasks/get` / `tasks/cancel` the task it started.
//!
//! Lineage alone cannot say "the user": a process that daemonizes is
//! reparented to init and looks like it. The token can.
//!
//! The proof is an HMAC of the target agent and method under the token, never
//! the token itself. The dialer finds the socket through the target's
//! `running.lock`, which that agent may rewrite, so whatever is sent may land
//! on a socket the agent controls. A proof bound to that agent's own name is
//! worthless anywhere else, and the token never leaves the home.

use std::io;
use std::path::Path;

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// The param that carries the proof. The runtime strips it before the
/// handler runs.
pub const PARAM: &str = "owner_proof";

/// Domain separator, so a proof can never double as any other MAC under the
/// same token.
const DOMAIN: &[u8] = b"mur-owner-call-v1\0";

/// Methods served only to the agent itself or its owner.
///
/// Not here, on purpose: peer-facing methods `accepts_from` already governs
/// (`message/send`, `agent/card`, `skills/get`, `channel/delegate`,
/// `channel/consult`); methods with their own credential (`tool/hitl_respond`,
/// `egress/grant`: the approval token; `shim/hello`: the shim ticket); and
/// `tools/*`, which is self-only — not even the owner runs an agent's tools
/// over its socket.
pub const OWNER_METHODS: &[&str] = &[
    "secret/set",
    "secret/delete",
    "model/set",
    "effort/set",
    "memory/reload",
    "tasks/get",
    "tasks/list",
    "tasks/cancel",
    "turn/steer",
    crate::hitl::HITL_LIST_METHOD,
    crate::hitl::HITL_FORGET_METHOD,
    crate::hitl::HITL_PENDING_METHOD,
    "tool/test_hitl_request",
];

pub fn is_owner_method(method: &str) -> bool {
    OWNER_METHODS.contains(&method)
}

/// The proof for calling `method` on `agent`, hex-encoded.
pub fn proof(token: &str, agent: &str, method: &str) -> String {
    hex::encode(mac(token, agent, method).finalize().into_bytes())
}

/// Whether `presented` is the proof for `method` on `agent`. Constant time.
pub fn verify(token: &str, agent: &str, method: &str, presented: &str) -> bool {
    let Ok(raw) = hex::decode(presented) else {
        return false;
    };
    let expected = mac(token, agent, method).finalize().into_bytes();
    raw.len() == expected.len() && bool::from(expected.as_slice().ct_eq(&raw))
}

fn mac(token: &str, agent: &str, method: &str) -> Hmac<Sha256> {
    let mut m =
        Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts a key of any length");
    m.update(DOMAIN);
    // Length-prefixed, so no choice of names can make two (agent, method)
    // pairs encode the same bytes.
    for field in [agent, method] {
        m.update(&(field.len() as u64).to_be_bytes());
        m.update(field.as_bytes());
    }
    m
}

/// Put the owner proof on `params` when `method` is an owner method.
///
/// Every other method is left alone, so the proof never travels further than
/// it must. When no token exists yet the params go out bare and the runtime's
/// refusal says why. `params` that are not an object (`null`) become one.
pub fn attach(params: &mut Value, mur_home: &Path, agent: &str, method: &str) -> io::Result<()> {
    if !is_owner_method(method) {
        return Ok(());
    }
    let Some(token) = crate::hitl::approval_token::read(mur_home)? else {
        return Ok(());
    };
    if params.is_null() {
        *params = Value::Object(Default::default());
    }
    if let Some(obj) = params.as_object_mut() {
        obj.insert(
            PARAM.to_string(),
            Value::String(proof(&token, agent, method)),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn a_proof_verifies_only_for_its_own_agent_and_method() {
        let p = proof(T, "alice", "secret/set");
        assert!(verify(T, "alice", "secret/set", &p));
        assert!(!verify(T, "bob", "secret/set", &p), "another agent");
        assert!(!verify(T, "alice", "model/set", &p), "another method");
        assert!(!verify("ff", "alice", "secret/set", &p), "another home");
        // No ambiguity at the separator: ("a", "b/c") is not ("a\0b", "c").
        assert_ne!(proof(T, "a", "b\0c"), proof(T, "a\0b", "c"));
    }

    #[test]
    fn malformed_proofs_are_refused() {
        let p = proof(T, "alice", "secret/set");
        for bad in ["", "zz", &p[..10], &format!("{p}00"), T] {
            assert!(!verify(T, "alice", "secret/set", bad), "{bad:?}");
        }
    }

    #[test]
    fn the_token_is_never_sent() {
        let home = tempfile::tempdir().unwrap();
        let token = crate::hitl::approval_token::load_or_create(home.path()).unwrap();
        let mut p = json!({ "name": "K", "value": "v" });
        attach(&mut p, home.path(), "alice", "secret/set").unwrap();
        let sent = p[PARAM].as_str().unwrap();
        assert!(verify(&token, "alice", "secret/set", sent));
        assert!(!p.to_string().contains(&token));
    }

    #[test]
    fn attach_touches_owner_methods_only() {
        let home = tempfile::tempdir().unwrap();
        crate::hitl::approval_token::load_or_create(home.path()).unwrap();
        for m in [
            "message/send",
            "agent/card",
            "channel/delegate",
            "tools/call",
        ] {
            let mut p = json!({});
            attach(&mut p, home.path(), "alice", m).unwrap();
            assert!(p.get(PARAM).is_none(), "{m}");
        }
        let mut null = Value::Null;
        attach(&mut null, home.path(), "alice", "memory/reload").unwrap();
        assert!(null[PARAM].is_string(), "null params become an object");
    }

    #[test]
    fn no_token_yet_sends_the_call_bare() {
        let home = tempfile::tempdir().unwrap();
        let mut p = json!({});
        attach(&mut p, home.path(), "alice", "tasks/cancel").unwrap();
        assert_eq!(p, json!({}));
    }
}
