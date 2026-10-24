use crate::constants::*;
use crate::credential::Credential;
use crate::error::BrokerError;
use sha2::Digest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerLimits {
    pub max_pack_bytes: u64,
    pub max_object_count: u64,
    pub max_blob_bytes: u64,
    pub max_index_wall_secs: u64,
    pub max_prefetch_bytes: u64,
    pub max_prefetch_wall_secs: u64,
    pub max_prefetch_disk_bytes: u64,
    pub max_concurrent_prefetch: usize,
    pub max_pending_per_agent: usize,
    pub max_new_requests_per_window: usize,
    pub request_window_secs: i64,
    pub git_timeout_secs: u64,
    pub push_timeout_secs: u64,
}
impl Default for BrokerLimits {
    fn default() -> Self {
        Self {
            max_pack_bytes: DEFAULT_MAX_PACK_BYTES,
            max_object_count: DEFAULT_MAX_OBJECT_COUNT,
            max_blob_bytes: DEFAULT_MAX_BLOB_BYTES,
            max_index_wall_secs: DEFAULT_MAX_INDEX_WALL_SECS,
            max_prefetch_bytes: DEFAULT_MAX_PREFETCH_BYTES,
            max_prefetch_wall_secs: DEFAULT_MAX_PREFETCH_WALL_SECS,
            max_prefetch_disk_bytes: DEFAULT_MAX_PREFETCH_DISK_BYTES,
            max_concurrent_prefetch: DEFAULT_MAX_CONCURRENT_PREFETCH,
            max_pending_per_agent: DEFAULT_MAX_PENDING_PER_AGENT,
            max_new_requests_per_window: DEFAULT_MAX_NEW_REQUESTS_PER_WINDOW,
            request_window_secs: DEFAULT_REQUEST_WINDOW_SECS,
            git_timeout_secs: DEFAULT_GIT_TIMEOUT_SECS,
            push_timeout_secs: DEFAULT_PUSH_TIMEOUT_SECS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePolicy {
    pub remote_id: String,
    pub canonical_remote_endpoint: String,
    pub creation_base_refs: Vec<String>,
    pub ref_prefix: String,
    /// Login for a network endpoint (path and username, never the secret). `None` for a local
    /// endpoint and for every policy that predates credentials.
    pub credential: Option<Credential>,
    /// Login used before approval (prefetch). `None` means `credential` is used for both.
    pub read_credential: Option<Credential>,
}
impl RemotePolicy {
    /// Add the credentials' identities to a digest input. Each only when present, so a policy
    /// without one hashes the exact JSON it did before that field existed and old approvals still
    /// verify. The read credential is its own key: swapping the two logins changes the digest.
    fn with_credential(&self, mut v: serde_json::Value) -> serde_json::Value {
        for (key, cred) in [
            ("credential", &self.credential),
            ("read_credential", &self.read_credential),
        ] {
            if let Some(c) = cred {
                v[key] = serde_json::json!({
                    "secret_path": c.secret_path.as_str(), "username": c.username.as_str(),
                });
            }
        }
        v
    }
    /// Hex SHA-256 of the JCS of the policy, base refs sorted so order cannot void approvals.
    pub fn digest(&self) -> String {
        let mut base = self.creation_base_refs.clone();
        base.sort();
        let v = self.with_credential(serde_json::json!({
            "remote_id": self.remote_id, "endpoint": self.canonical_remote_endpoint,
            "ref_prefix": self.ref_prefix, "creation_base_refs": base,
        }));
        let jcs = serde_jcs::to_string(&v).expect("jcs of a json value");
        hex::encode(sha2::Sha256::digest(jcs.as_bytes()))
    }
    /// Digest of what the remote *is* (id + endpoint + login), apart from what may be pushed to it.
    pub fn remote_digest(&self) -> String {
        let v = self.with_credential(serde_json::json!({
            "remote_id": self.remote_id, "endpoint": self.canonical_remote_endpoint,
        }));
        let jcs = serde_jcs::to_string(&v).expect("jcs of a json value");
        hex::encode(sha2::Sha256::digest(jcs.as_bytes()))
    }
    pub fn validate_enrollment(&self) -> Result<(), BrokerError> {
        let bad = |m: &str| Err(BrokerError::InvalidRequest(m.into()));
        if !self.ref_prefix.starts_with(ALLOWED_REF_PREFIX) || !self.ref_prefix.ends_with('/') {
            return bad("ref_prefix");
        }
        for r in &self.creation_base_refs {
            let tail = r.strip_prefix("refs/heads/").filter(|t| !t.is_empty());
            let ok = tail.is_some_and(|t| {
                !t.contains("..")
                    && !t.contains("//")
                    && !t.ends_with('/')
                    && !t.ends_with(".lock")
                    && !t.starts_with('.')
                    && t.bytes()
                        .all(|b| b.is_ascii_graphic() && !b"~^:?*[\\".contains(&b))
            });
            if !ok {
                return bad("creation_base_ref");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> RemotePolicy {
        RemotePolicy {
            remote_id: "origin".into(),
            canonical_remote_endpoint: "https://h/r.git".into(),
            creation_base_refs: vec!["refs/heads/main".into()],
            ref_prefix: ALLOWED_REF_PREFIX.into(),
            credential: None,
            read_credential: None,
        }
    }
    #[test]
    fn creation_base_refs_must_be_full_heads() {
        let mut p = policy();
        p.creation_base_refs = vec!["main".into()];
        assert!(p.validate_enrollment().is_err());
        p.creation_base_refs = vec!["refs/tags/v1".into()];
        assert!(p.validate_enrollment().is_err());
        p.creation_base_refs = vec!["refs/heads/main".into()];
        assert!(p.validate_enrollment().is_ok());
        p.creation_base_refs = vec![];
        assert!(p.validate_enrollment().is_ok(), "empty is allowed");
    }
    #[test]
    fn changing_creation_base_refs_changes_the_digest() {
        let a = policy().digest();
        let mut p = policy();
        p.creation_base_refs.push("refs/heads/dev".into());
        assert_ne!(a, p.digest());
    }
    #[test]
    fn digest_ignores_creation_base_ref_order() {
        let mut a = policy();
        a.creation_base_refs = vec!["refs/heads/a".into(), "refs/heads/b".into()];
        let mut b = a.clone();
        b.creation_base_refs.reverse();
        assert_eq!(a.digest(), b.digest());
    }
    #[test]
    fn ref_prefix_cannot_widen_beyond_agent_namespace() {
        let mut p = policy();
        p.ref_prefix = "refs/heads/".into();
        assert!(p.validate_enrollment().is_err());
        p.ref_prefix = "refs/heads/agent/alice/".into();
        assert!(p.validate_enrollment().is_ok());
    }
    #[test]
    fn limits_default_to_constants() {
        assert_eq!(
            BrokerLimits::default().max_pack_bytes,
            DEFAULT_MAX_PACK_BYTES
        );
    }

    fn cred(path: &str, user: &str) -> crate::credential::Credential {
        crate::credential::Credential {
            secret_path: crate::credential::SecretPath::parse(path).unwrap(),
            username: crate::credential::Username::parse(user).unwrap(),
        }
    }
    #[test]
    fn a_policy_without_a_credential_keeps_the_digests_it_had_before_credentials_existed() {
        // Pinned from the code at 6c5bceaa1, before `credential` was a field: an approval signed
        // then must still verify, so the no-credential JSON shape may not gain a key.
        let p = policy();
        assert_eq!(
            p.digest(),
            "8c8b17a21109a5a93d25c5804d5a2710e1f6bea7c5ffb8924312d30af2419a22"
        );
        assert_eq!(
            p.remote_digest(),
            "3d00dc20aa8f273b3d34a663b6e5375e955479f1b160b9f37319096c2e51fe5f"
        );
    }
    #[test]
    fn a_policy_with_only_a_write_credential_keeps_the_digests_it_had_before_read_credentials() {
        // Pinned from the code at 08ba2bfe1, before `read_credential` was a field: a policy that
        // carries a credential and no read credential must hash the exact JSON it did, so an
        // approval signed then still verifies.
        let mut p = policy();
        p.credential = Some(cred("/s/t", "bot"));
        assert_eq!(
            p.digest(),
            "b0d4f03145d94e3750983305ae59821e1e2352f784fbcbabdcecdc859b856d16"
        );
        assert_eq!(
            p.remote_digest(),
            "95ad59e74a631d23b42281f76d66e1bdd98e98a926186b40ab793460ae6f85dc"
        );
    }
    #[test]
    fn a_read_credential_changes_both_digests_and_each_of_its_fields_counts() {
        let mut base = policy();
        base.credential = Some(cred("/s/t", "bot"));
        let mut with_read = base.clone();
        with_read.read_credential = Some(cred("/s/r", "reader"));
        let mut other_path = base.clone();
        other_path.read_credential = Some(cred("/s/r2", "reader"));
        let mut other_user = base.clone();
        other_user.read_credential = Some(cred("/s/r", "reader2"));
        let all = [&base, &with_read, &other_path, &other_user];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.digest(), b.digest());
                assert_ne!(a.remote_digest(), b.remote_digest());
            }
        }
    }
    #[test]
    fn the_read_credential_is_its_own_key_not_a_swap_with_the_write_credential() {
        let mut a = policy();
        a.credential = Some(cred("/s/t", "bot"));
        a.read_credential = Some(cred("/s/r", "reader"));
        let mut swapped = policy();
        swapped.credential = Some(cred("/s/r", "reader"));
        swapped.read_credential = Some(cred("/s/t", "bot"));
        assert_ne!(a.digest(), swapped.digest());
        assert_ne!(a.remote_digest(), swapped.remote_digest());
    }
    #[test]
    fn adding_a_credential_changes_both_digests() {
        let bare = policy();
        let mut p = policy();
        p.credential = Some(cred("/s/t", "bot"));
        assert_ne!(p.digest(), bare.digest());
        assert_ne!(p.remote_digest(), bare.remote_digest());
    }
    #[test]
    fn changing_the_secret_path_or_the_username_changes_both_digests() {
        let mut base = policy();
        base.credential = Some(cred("/s/t", "bot"));
        let mut other_path = policy();
        other_path.credential = Some(cred("/s/other", "bot"));
        let mut other_user = policy();
        other_user.credential = Some(cred("/s/t", "someone"));
        for p in [&other_path, &other_user] {
            assert_ne!(p.digest(), base.digest());
            assert_ne!(p.remote_digest(), base.remote_digest());
        }
    }
    #[test]
    fn the_same_credential_digests_the_same() {
        let mut a = policy();
        a.credential = Some(cred("/s/t", "bot"));
        let mut b = policy();
        b.credential = Some(cred("/s/t", "bot"));
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.remote_digest(), b.remote_digest());
    }
}
