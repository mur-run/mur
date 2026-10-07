use crate::constants::*;
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
}
impl RemotePolicy {
    /// Hex SHA-256 of the JCS of the policy, base refs sorted so order cannot void approvals.
    pub fn digest(&self) -> String {
        let mut base = self.creation_base_refs.clone();
        base.sort();
        let v = serde_json::json!({
            "remote_id": self.remote_id, "endpoint": self.canonical_remote_endpoint,
            "ref_prefix": self.ref_prefix, "creation_base_refs": base,
        });
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
}
