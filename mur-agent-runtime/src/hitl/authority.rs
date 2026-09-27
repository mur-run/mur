//! Who may approve: the check `tool/hitl_respond` runs on every `allow`.
//!
//! The agent's socket is reachable by the agent and anything it spawns, so a
//! bare `allow: true` proves nothing. An allow is honoured only when the
//! kernel sandbox is enforcing (otherwise `secrets/` is readable and the token
//! proves nothing either) AND it carries the home's approval token
//! (`mur_common::hitl::approval_token`), which the runtime read before sealing.
//! A deny needs neither: refusing is always safe.

use subtle::ConstantTimeEq;

/// What the runtime holds for checking approvals, fixed at boot.
#[derive(Clone)]
pub struct ApprovalAuthority {
    token: Option<String>,
    sandbox_enforcing: bool,
}

/// Why an `allow` was not honoured. The text goes back to the sender verbatim,
/// so it names the fix and never echoes the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The kernel sandbox is not enforcing, so any secret the agent could be
    /// kept from is readable after all.
    SandboxNotEnforcing,
    /// The runtime could not load the token before sealing.
    NoToken,
    /// The allow came without a token, or with the wrong one.
    BadToken,
}

impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Self::SandboxNotEnforcing => {
                "this agent's sandbox is not enforcing, so no approval can be \
                 trusted on this boot; the call stays denied. Run \
                 `mur agent perm list-paths <agent>` to see the seal state"
            }
            Self::NoToken => {
                "this agent could not load the approval token when it started, \
                 so it cannot accept approvals; the call stays denied. Check \
                 the agent log, then restart the agent"
            }
            Self::BadToken => {
                "the approval did not carry this MUR home's approval token, so \
                 it was not accepted; the call stays denied. Approve from the \
                 Hub or `mur agent cli` running as the same user and MUR_HOME"
            }
        }
    }
}

impl ApprovalAuthority {
    pub fn new(token: Option<String>, sandbox_enforcing: bool) -> Self {
        Self {
            token,
            sandbox_enforcing,
        }
    }

    /// Refuses every allow. Test and fallback default.
    pub fn deny_all() -> Self {
        Self::new(None, false)
    }

    /// Decide whether an answer may release the gate.
    pub fn check(&self, allow: bool, presented: Option<&str>) -> Result<(), Refusal> {
        if !allow {
            return Ok(());
        }
        if !self.sandbox_enforcing {
            return Err(Refusal::SandboxNotEnforcing);
        }
        let expected = self.token.as_deref().ok_or(Refusal::NoToken)?;
        let presented = presented.ok_or(Refusal::BadToken)?;
        // Length leaks nothing: every valid token is the same length.
        if bool::from(expected.as_bytes().ct_eq(presented.as_bytes())) {
            Ok(())
        } else {
            Err(Refusal::BadToken)
        }
    }
}

impl std::fmt::Debug for ApprovalAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalAuthority")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("sandbox_enforcing", &self.sandbox_enforcing)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "aa11";

    #[test]
    fn a_deny_always_passes() {
        for a in [
            ApprovalAuthority::deny_all(),
            ApprovalAuthority::new(Some(T.into()), true),
        ] {
            assert_eq!(a.check(false, None), Ok(()));
            assert_eq!(a.check(false, Some("wrong")), Ok(()));
        }
    }

    #[test]
    fn an_allow_needs_the_token_and_a_sealed_sandbox() {
        let sealed = ApprovalAuthority::new(Some(T.into()), true);
        assert_eq!(sealed.check(true, Some(T)), Ok(()));
        assert_eq!(sealed.check(true, None), Err(Refusal::BadToken));
        assert_eq!(sealed.check(true, Some("")), Err(Refusal::BadToken));
        assert_eq!(sealed.check(true, Some("aa12")), Err(Refusal::BadToken));
        assert_eq!(sealed.check(true, Some("aa11aa11")), Err(Refusal::BadToken));

        let advisory = ApprovalAuthority::new(Some(T.into()), false);
        assert_eq!(
            advisory.check(true, Some(T)),
            Err(Refusal::SandboxNotEnforcing),
            "a correct token proves nothing when secrets/ is readable"
        );

        let tokenless = ApprovalAuthority::new(None, true);
        assert_eq!(tokenless.check(true, Some(T)), Err(Refusal::NoToken));
    }

    #[test]
    fn debug_never_prints_the_token() {
        let s = format!("{:?}", ApprovalAuthority::new(Some(T.into()), true));
        assert!(!s.contains(T), "{s}");
    }
}
