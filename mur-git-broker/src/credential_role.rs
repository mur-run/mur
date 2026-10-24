//! Which enrolled credential the broker selected for a phase (spec #2093 §3, §4).
//!
//! The selection is a pure function of the [`Bound`] it is given: it reads `credential` and
//! `read_credential` and nothing else, so the pre-approval path cannot reach the write secret
//! by any expression when a read credential exists.

use crate::binding::Bound;
use crate::credential::Credential;
use crate::error::BrokerError;

/// When the broker needs a login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before any human approval: the read-only prefetch.
    Prefetch,
    /// After approval: the push.
    Push,
}

/// Which enrolled credential was selected. Recorded in the audit trail as the *selection*, never
/// as a verified permission of the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRole {
    /// The read credential, used for the prefetch of an enrollment that has one.
    Read,
    /// The write credential, used for the push of an enrollment that has a read credential.
    Write,
    /// The one credential of an enrollment with no read credential, used for both phases.
    Shared,
}

impl CredentialRole {
    const READ: &'static str = "read";
    const WRITE: &'static str = "write";
    const SHARED: &'static str = "shared";

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => Self::READ,
            Self::Write => Self::WRITE,
            Self::Shared => Self::SHARED,
        }
    }

    /// The inverse of [`Self::as_str`]. An unknown value is a storage error, not a guess.
    pub fn parse(s: &str) -> Result<Self, BrokerError> {
        match s {
            Self::READ => Ok(Self::Read),
            Self::WRITE => Ok(Self::Write),
            Self::SHARED => Ok(Self::Shared),
            _ => Err(BrokerError::Storage("unknown audit credential role".into())),
        }
    }
}

/// The credential `phase` must use, and the role to audit it under. `None`: nothing to open.
///
/// | read credential | `Prefetch`            | `Push`                |
/// |-----------------|-----------------------|-----------------------|
/// | present         | `read_credential`, Read | `credential`, Write |
/// | absent          | `credential`, Shared  | `credential`, Shared  |
pub fn select(bound: &Bound, phase: Phase) -> Option<(&Credential, CredentialRole)> {
    match (&bound.read_credential, phase) {
        (Some(read), Phase::Prefetch) => Some((read, CredentialRole::Read)),
        (Some(_), Phase::Push) => bound
            .credential
            .as_ref()
            .map(|c| (c, CredentialRole::Write)),
        (None, _) => bound
            .credential
            .as_ref()
            .map(|c| (c, CredentialRole::Shared)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{SecretPath, Username};
    use crate::policy::RemotePolicy;

    fn cred(path: &str) -> Credential {
        Credential {
            secret_path: SecretPath::parse(path).unwrap(),
            username: Username::parse("u").unwrap(),
        }
    }
    fn bound(write: Option<&str>, read: Option<&str>) -> Bound {
        Bound {
            remote_url: "https://h/r.git".into(),
            // The policy deliberately names other secrets: the selector must not read it.
            policy: RemotePolicy {
                remote_id: "o".into(),
                canonical_remote_endpoint: "https://h/r.git".into(),
                creation_base_refs: vec![],
                ref_prefix: "refs/heads/agent/".into(),
                credential: Some(cred("/policy/write")),
                read_credential: Some(cred("/policy/read")),
            },
            credential: write.map(cred),
            read_credential: read.map(cred),
        }
    }
    fn picked(b: &Bound, phase: Phase) -> Option<(String, CredentialRole)> {
        select(b, phase).map(|(c, r)| (c.secret_path.as_str().to_string(), r))
    }

    #[test]
    fn with_a_read_credential_prefetch_reads_and_push_writes() {
        let b = bound(Some("/w"), Some("/r"));
        assert_eq!(
            picked(&b, Phase::Prefetch),
            Some(("/r".into(), CredentialRole::Read))
        );
        assert_eq!(
            picked(&b, Phase::Push),
            Some(("/w".into(), CredentialRole::Write))
        );
    }

    #[test]
    fn without_a_read_credential_both_phases_use_the_one_credential_as_shared() {
        let b = bound(Some("/w"), None);
        for phase in [Phase::Prefetch, Phase::Push] {
            assert_eq!(
                picked(&b, phase),
                Some(("/w".into(), CredentialRole::Shared))
            );
        }
    }

    #[test]
    fn no_credential_selects_nothing_in_either_phase() {
        let b = bound(None, None);
        for phase in [Phase::Prefetch, Phase::Push] {
            assert_eq!(picked(&b, phase), None);
        }
    }

    #[test]
    fn the_selector_reads_the_bound_fields_not_the_policy() {
        // `bound()` puts different secrets in `policy`; none of them may ever be selected.
        for b in [bound(Some("/w"), Some("/r")), bound(Some("/w"), None)] {
            for phase in [Phase::Prefetch, Phase::Push] {
                let (path, _) = picked(&b, phase).unwrap();
                assert!(!path.starts_with("/policy/"), "{path}");
            }
        }
    }

    #[test]
    fn a_role_round_trips_and_an_unknown_one_is_a_storage_error() {
        for r in [
            CredentialRole::Read,
            CredentialRole::Write,
            CredentialRole::Shared,
        ] {
            assert_eq!(CredentialRole::parse(r.as_str()).unwrap(), r);
        }
        assert!(matches!(
            CredentialRole::parse("admin"),
            Err(BrokerError::Storage(_))
        ));
    }
}
