//! #2488 guard: every method `build_dispatcher` registers must be placed in
//! exactly one access class, so a new method cannot reach peers by default.
//! Read from the source because the real dispatcher needs a whole runtime.

/// Methods `accepts_from` governs on its own: conversation with peers.
const PEER: &[&str] = &[
    "agent/card",
    "message/send",
    "skills/get",
    "channel/delegate",
    mur_common::build::CHANNEL_CONSULT_METHOD,
];
/// Methods carrying their own credential (approval token / shim ticket).
const OWN_CREDENTIAL: &[&str] = &[
    "tool/hitl_respond",
    "shim/hello",
    mur_common::egress_event::GRANT_METHOD,
];
/// Served only to the agent's own lineage.
const SELF_ONLY: &[&str] = &["tools/list", "tools/call"];

/// The method names registered in `dispatch.rs`, resolving the constants it
/// registers by path.
fn registered() -> Vec<String> {
    let src = include_str!("dispatch.rs");
    let mut out = Vec::new();
    for chunk in src.split("d.register(").skip(1) {
        let first = chunk.trim_start().split([',', ')']).next().unwrap().trim();
        let name = if let Some(lit) = first.strip_prefix('"') {
            lit.trim_end_matches('"').to_string()
        } else {
            match first.rsplit("::").next().unwrap() {
                "CHANNEL_CONSULT_METHOD" => mur_common::build::CHANNEL_CONSULT_METHOD,
                "GRANT_METHOD" => mur_common::egress_event::GRANT_METHOD,
                "HITL_LIST_METHOD" => mur_common::hitl::HITL_LIST_METHOD,
                "HITL_FORGET_METHOD" => mur_common::hitl::HITL_FORGET_METHOD,
                "HITL_PENDING_METHOD" => mur_common::hitl::HITL_PENDING_METHOD,
                other => panic!("unknown method constant {other}: add it here and classify it"),
            }
            .to_string()
        };
        out.push(name);
    }
    out
}

#[test]
fn every_registered_method_has_exactly_one_access_class() {
    let methods = registered();
    assert!(methods.len() >= 20, "parser found too few: {methods:?}");
    for m in &methods {
        let classes = [
            PEER.contains(&m.as_str()),
            OWN_CREDENTIAL.contains(&m.as_str()),
            SELF_ONLY.contains(&m.as_str()),
            mur_common::owner_call::is_owner_method(m),
        ];
        assert_eq!(
            classes.iter().filter(|c| **c).count(),
            1,
            "{m}: classify it as peer, own-credential, self-only, or \
             mur_common::owner_call::OWNER_METHODS"
        );
    }
    for m in mur_common::owner_call::OWNER_METHODS {
        assert!(
            methods.iter().any(|r| r == m),
            "{m} is listed but not registered"
        );
    }
}
