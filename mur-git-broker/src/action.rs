use crate::constants::*;
use crate::error::BrokerError;
use crate::oid::*;
use sha2::Digest;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefUpdate {
    pub r#ref: String,
    pub old_sha: String,
    pub new_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDocument {
    pub version: String,
    pub agent_id: String,
    pub task_id: String,
    pub enrollment_epoch: u64,
    pub request_id: String,
    pub repo_id: String,
    pub repo_identity: String,
    pub object_format: ObjectFormat,
    pub remote_id: String,
    pub canonical_remote_endpoint: String,
    pub remote_policy_digest: String,
    pub ref_policy_digest: String,
    pub updates: Vec<RefUpdate>,
    pub force: bool,
    pub delete: bool,
}
impl ActionDocument {
    pub fn validate(&self) -> Result<(), BrokerError> {
        if self.version != ACTION_VERSION || self.force || self.delete || self.updates.len() != 1 {
            return Err(BrokerError::InvalidRequest("action".into()));
        }
        let u = &self.updates[0];
        validate_ref(&u.r#ref)?;
        validate_oid(&u.old_sha, self.object_format)?;
        validate_oid(&u.new_sha, self.object_format)?;
        if u.new_sha == zero_oid(self.object_format) {
            return Err(BrokerError::InvalidRequest("delete".into()));
        }
        Ok(())
    }
    pub fn is_creation(&self) -> bool {
        self.updates[0].old_sha == zero_oid(self.object_format)
    }
    pub fn action_hash(&self) -> Result<String, BrokerError> {
        self.validate()?;
        let jcs = serde_jcs::to_string(self).map_err(|e| BrokerError::Storage(e.to_string()))?;
        let mut h = sha2::Sha256::new();
        h.update(ACTION_HASH_DOMAIN.as_bytes());
        h.update(jcs.as_bytes());
        Ok(hex::encode(h.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc() -> ActionDocument {
        ActionDocument {
            version: ACTION_VERSION.into(),
            agent_id: "agent-a".into(),
            task_id: "task-1".into(),
            enrollment_epoch: 1,
            request_id: "req-1".into(),
            repo_id: "repo-1".into(),
            repo_identity: "ident-1".into(),
            object_format: ObjectFormat::Sha1,
            remote_id: "origin".into(),
            canonical_remote_endpoint: "https://h/r.git".into(),
            remote_policy_digest: "rpd-1".into(),
            ref_policy_digest: "refpd-1".into(),
            updates: vec![RefUpdate {
                r#ref: "refs/heads/agent/task-1".into(),
                old_sha: "a".repeat(40),
                new_sha: "b".repeat(40),
            }],
            force: false,
            delete: false,
        }
    }
    #[test]
    fn hash_is_stable_and_domain_separated() {
        use sha2::{Digest, Sha256};
        let d = doc();
        let h = d.action_hash().unwrap();
        assert_eq!(h, d.clone().action_hash().unwrap());
        let plain = hex::encode(Sha256::digest(serde_jcs::to_string(&d).unwrap().as_bytes()));
        assert_ne!(h, plain, "the domain tag must be part of the preimage");
    }
    #[test]
    fn every_field_changes_the_hash() {
        let base = doc().action_hash().unwrap();
        let mut variants: Vec<ActionDocument> = Vec::new();
        macro_rules! v {
            ($f:ident, $val:expr) => {{
                let mut d = doc();
                d.$f = $val;
                variants.push(d);
            }};
        }
        v!(agent_id, "other".into());
        v!(task_id, "other".into());
        v!(enrollment_epoch, 99);
        v!(request_id, "other".into());
        v!(repo_id, "other".into());
        v!(repo_identity, "other".into());
        v!(remote_id, "other".into());
        v!(canonical_remote_endpoint, "other".into());
        v!(remote_policy_digest, "other".into());
        v!(ref_policy_digest, "other".into());
        let mut d = doc();
        d.updates[0].new_sha = "d".repeat(40);
        variants.push(d);
        let mut d = doc();
        d.updates[0].old_sha = "c".repeat(40);
        variants.push(d);
        for d in variants {
            assert_ne!(d.action_hash().unwrap(), base);
        }
    }
    #[test]
    fn field_order_in_input_json_does_not_matter() {
        let d = doc();
        // serde_json::Map is sorted (no preserve_order), so build the reversed text by hand.
        let v = serde_json::to_value(&d).unwrap();
        let fwd: Vec<String> = v
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, val)| format!("{}:{}", serde_json::to_string(k).unwrap(), val))
            .collect();
        let rev_text = format!(
            "{{{}}}",
            fwd.iter().rev().cloned().collect::<Vec<_>>().join(",")
        );
        assert_ne!(
            rev_text,
            format!("{{{}}}", fwd.join(",")),
            "the input order must actually differ"
        );
        let reordered: ActionDocument = serde_json::from_str(&rev_text).unwrap();
        assert_eq!(reordered.action_hash().unwrap(), d.action_hash().unwrap());
    }
    #[test]
    fn extra_fields_and_multi_update_are_rejected() {
        let mut v = serde_json::to_value(doc()).unwrap();
        v["surprise"] = 1.into();
        assert!(serde_json::from_value::<ActionDocument>(v).is_err());
        let mut d = doc();
        d.updates.push(d.updates[0].clone());
        assert!(d.validate().is_err(), "v1 allows exactly one update");
    }
    #[test]
    fn force_and_delete_must_be_false() {
        let mut d = doc();
        d.force = true;
        assert!(d.validate().is_err());
        let mut d = doc();
        d.delete = true;
        assert!(d.validate().is_err());
    }
}
