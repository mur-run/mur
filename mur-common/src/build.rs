//! Compile-time build identity. `SHORT_SHA` is the git commit the binary was
//! built from (set by build.rs), or "unknown" for git-less builds (crates.io).
//! Used to detect when a running agent's binary differs from the installed one.

/// 12-char git sha of this build, or "unknown".
pub const SHORT_SHA: &str = env!("MUR_GIT_SHA");

/// A2A method-surface version. Bump ONLY on incompatible change to a dialed
/// method (added method, changed params/result contract). Carried in
/// `running.lock` AgentCard; dial refuses a method whose
/// `method_min_proto` exceeds the peer's advertised proto.
pub const A2A_PROTO_VERSION: u32 = 1;

/// Minimum proto a peer must advertise to accept the dialed `method`. `0` means
/// always available (never gated). Add an entry per method introduced/changed.
pub fn method_min_proto(method: &str) -> u32 {
    match method {
        "channel/delegate" => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sha_is_set() {
        // Either a real 12-char hex sha, or the "unknown" fallback.
        assert!(
            SHORT_SHA == "unknown" || SHORT_SHA.len() == 12,
            "got {SHORT_SHA:?}"
        );
    }

    #[test]
    fn method_min_proto_gates_channel_delegate_only() {
        // channel/delegate requires the proto that introduced it.
        assert_eq!(method_min_proto("channel/delegate"), 1);
        // Always-available methods are ungated (min 0).
        assert_eq!(method_min_proto("message/send"), 0);
        assert_eq!(method_min_proto("agent/card"), 0);
        // The current proto is at least the highest gated method.
        const { assert!(A2A_PROTO_VERSION >= 1) };
    }
}
