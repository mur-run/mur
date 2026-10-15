//! Enrollment records: the human's decision about which agent may ask to push where.
//!
//! `<mur_home>/git-push/private/enrollments.yaml` is the single source of truth. The broker
//! reads only this file; `registry.yaml` (what the agent-side tool reads) is derived from it by
//! [`Enrollments::registry_yaml`], so a crash between the two writes leaves a stale registry
//! and never a wider enrollment.
//!
//! Two layers, deliberately separate:
//! - **repo** (`repos.<id>`): what the repo *is*: its path, object format and a random
//!   `identity`. Repointing a repo mints a new identity, which voids every pending push for that
//!   repo whichever agent asked.
//! - **enrollment** (agent, repo, remote): the policy and an `epoch`. Changing the policy mints
//!   a new epoch.
//!
//! Loading is fail-closed: any record that does not validate makes the whole file an `Err`.

use crate::constants::*;
use crate::credential::Credential;
use crate::error::BrokerError;
use crate::oid::ObjectFormat;
use crate::policy::RemotePolicy;
use crate::request_file::validate_id;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

fn bad(m: impl Into<String>) -> BrokerError {
    BrokerError::InvalidRequest(m.into())
}

pub use crate::endpoint::Endpoint;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoRecord {
    /// Random 128-bit hex minted when this id is bound to a path.
    pub identity: String,
    pub path: PathBuf,
    pub object_format: ObjectFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentRecord {
    pub agent: String,
    pub repo_id: String,
    pub remote_id: String,
    pub endpoint: Endpoint,
    pub ref_prefix: String,
    #[serde(default)]
    pub creation_base_refs: Vec<String>,
    /// Login for a network endpoint. Absent in every record written before https existed, and
    /// omitted again on write when `None`, so those files round-trip byte for byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<Credential>,
    pub epoch: u64,
}

impl EnrollmentRecord {
    pub fn policy(&self) -> RemotePolicy {
        RemotePolicy {
            remote_id: self.remote_id.clone(),
            canonical_remote_endpoint: self.endpoint.as_str().to_string(),
            creation_base_refs: self.creation_base_refs.clone(),
            ref_prefix: self.ref_prefix.clone(),
        }
    }
    fn same_key(&self, agent: &str, repo_id: &str, remote_id: &str) -> bool {
        self.agent == agent && self.repo_id == repo_id && self.remote_id == remote_id
    }
}

/// What a human asked for. `repo` is only consulted the first time a `repo_id` appears.
#[derive(Debug, Clone)]
pub struct EnrollRequest {
    pub agent: String,
    pub repo_id: String,
    pub remote_id: String,
    pub endpoint: Endpoint,
    pub ref_prefix: String,
    pub creation_base_refs: Vec<String>,
    pub credential: Option<Credential>,
    pub new_repo: Option<NewRepo>,
}
#[derive(Debug, Clone)]
pub struct NewRepo {
    pub path: PathBuf,
    pub object_format: ObjectFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollOutcome {
    Created,
    Unchanged,
    /// The policy changed in place; pending pushes under the old epoch become `policy_changed`.
    Updated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enrollments {
    pub version: u32,
    /// Next epoch to hand out. Never decreases, so a revoked-then-recreated enrollment cannot
    /// reuse an epoch a stale pending row still carries.
    pub next_epoch: u64,
    #[serde(default)]
    pub repos: BTreeMap<String, RepoRecord>,
    #[serde(default)]
    pub enrollments: Vec<EnrollmentRecord>,
}

/// The shape `registry.yaml` has always had; the agent-side reader denies unknown fields.
#[derive(Serialize)]
struct RegistryOut<'a> {
    repos: BTreeMap<&'a str, RegistryEntryOut<'a>>,
}
#[derive(Serialize)]
struct RegistryEntryOut<'a> {
    path: &'a Path,
    remotes: Vec<&'a str>,
}

impl Enrollments {
    /// An empty file whose first epoch is `seed`. Callers seed from the wall clock so that a
    /// deleted file cannot restart epochs at a value an old pending row already holds.
    pub fn empty(seed: u64) -> Self {
        Self {
            version: ENROLLMENTS_VERSION,
            next_epoch: seed.max(1),
            repos: BTreeMap::new(),
            enrollments: Vec::new(),
        }
    }

    /// Missing file ⇒ `None` (nothing enrolled). Present but unreadable or invalid ⇒ `Err`.
    pub fn load(path: &Path) -> Result<Option<Self>, BrokerError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(BrokerError::Storage(format!("enrollments: {}", e.kind()))),
        };
        let me: Self = serde_yaml_ng::from_str(&text)
            .map_err(|e| BrokerError::Storage(format!("enrollments: {e}")))?;
        me.validate()?;
        Ok(Some(me))
    }

    pub fn validate(&self) -> Result<(), BrokerError> {
        if self.version != ENROLLMENTS_VERSION {
            return Err(bad("enrollments_version"));
        }
        for (id, r) in &self.repos {
            validate_id(id, "repo_id").map_err(|_| bad("repo_id"))?;
            if !valid_identity(&r.identity) || !r.path.is_absolute() {
                return Err(bad("repo_record"));
            }
        }
        let mut seen = BTreeSet::new();
        for e in &self.enrollments {
            validate_id(&e.agent, "agent").map_err(|_| bad("agent"))?;
            validate_id(&e.repo_id, "repo_id").map_err(|_| bad("repo_id"))?;
            validate_id(&e.remote_id, "remote_id").map_err(|_| bad("remote_id"))?;
            if !self.repos.contains_key(&e.repo_id) {
                return Err(bad("unknown_repo"));
            }
            if e.epoch == 0 || e.epoch >= self.next_epoch {
                return Err(bad("epoch"));
            }
            e.policy().validate_enrollment()?;
            if !seen.insert((&e.agent, &e.repo_id, &e.remote_id)) {
                return Err(bad("duplicate_enrollment"));
            }
        }
        Ok(())
    }

    pub fn find(&self, agent: &str, repo_id: &str, remote_id: &str) -> Option<&EnrollmentRecord> {
        self.enrollments
            .iter()
            .find(|e| e.same_key(agent, repo_id, remote_id))
    }

    fn mint_epoch(&mut self) -> u64 {
        let e = self.next_epoch;
        self.next_epoch += 1;
        e
    }

    /// Insert or update one enrollment. All validation happens before any change, so an `Err`
    /// leaves `self` untouched.
    pub fn enroll(&mut self, req: EnrollRequest) -> Result<EnrollOutcome, BrokerError> {
        validate_id(&req.agent, "agent").map_err(|_| bad("agent"))?;
        validate_id(&req.repo_id, "repo_id").map_err(|_| bad("repo_id"))?;
        validate_id(&req.remote_id, "remote_id").map_err(|_| bad("remote_id"))?;
        let mut bases = req.creation_base_refs.clone();
        bases.sort();
        bases.dedup();
        let rec = EnrollmentRecord {
            agent: req.agent,
            repo_id: req.repo_id,
            remote_id: req.remote_id,
            endpoint: req.endpoint,
            ref_prefix: req.ref_prefix,
            creation_base_refs: bases,
            credential: req.credential,
            epoch: 0,
        };
        rec.policy().validate_enrollment()?;
        match self.repos.get(&rec.repo_id) {
            Some(existing) => {
                if let Some(n) = &req.new_repo
                    && (n.path != existing.path || n.object_format != existing.object_format)
                {
                    return Err(bad("repo_differs"));
                }
            }
            None => {
                let n = req.new_repo.ok_or_else(|| bad("repo_unknown"))?;
                if !n.path.is_absolute() {
                    return Err(bad("repo_path"));
                }
                self.repos.insert(
                    rec.repo_id.clone(),
                    RepoRecord {
                        identity: new_identity(),
                        path: n.path,
                        object_format: n.object_format,
                    },
                );
            }
        }
        let idx = self
            .enrollments
            .iter()
            .position(|e| e.same_key(&rec.agent, &rec.repo_id, &rec.remote_id));
        match idx {
            Some(i) => {
                let old = &self.enrollments[i];
                if old.policy().digest() == rec.policy().digest()
                    && old.credential == rec.credential
                {
                    return Ok(EnrollOutcome::Unchanged);
                }
                let epoch = self.mint_epoch();
                self.enrollments[i] = EnrollmentRecord { epoch, ..rec };
                Ok(EnrollOutcome::Updated)
            }
            None => {
                let epoch = self.mint_epoch();
                self.enrollments.push(EnrollmentRecord { epoch, ..rec });
                Ok(EnrollOutcome::Created)
            }
        }
    }

    /// Remove one enrollment; a repo with none left is removed too. `false` if there was none.
    pub fn revoke(&mut self, agent: &str, repo_id: &str, remote_id: &str) -> bool {
        let before = self.enrollments.len();
        self.enrollments
            .retain(|e| !e.same_key(agent, repo_id, remote_id));
        let removed = self.enrollments.len() != before;
        if removed && !self.enrollments.iter().any(|e| e.repo_id == repo_id) {
            self.repos.remove(repo_id);
        }
        removed
    }

    /// Repoint an existing repo. Always mints a new identity, so pending pushes for it void.
    pub fn move_repo(&mut self, repo_id: &str, to: NewRepo) -> Result<(), BrokerError> {
        if !to.path.is_absolute() {
            return Err(bad("repo_path"));
        }
        let r = self
            .repos
            .get_mut(repo_id)
            .ok_or_else(|| bad("repo_unknown"))?;
        *r = RepoRecord {
            identity: new_identity(),
            path: to.path,
            object_format: to.object_format,
        };
        Ok(())
    }

    /// The text `registry.yaml` must contain.
    pub fn registry_yaml(&self) -> Result<String, BrokerError> {
        let mut repos = BTreeMap::new();
        for (id, r) in &self.repos {
            let mut remotes: Vec<&str> = self
                .enrollments
                .iter()
                .filter(|e| &e.repo_id == id)
                .map(|e| e.remote_id.as_str())
                .collect();
            remotes.sort();
            remotes.dedup();
            repos.insert(
                id.as_str(),
                RegistryEntryOut {
                    path: &r.path,
                    remotes,
                },
            );
        }
        serde_yaml_ng::to_string(&RegistryOut { repos })
            .map_err(|e| BrokerError::Storage(e.to_string()))
    }

    /// The binding the broker needs to accept (or void) a request.
    pub fn binding(
        &self,
        agent: &str,
        repo_id: &str,
        remote_id: &str,
    ) -> Option<RemoteBinding<'_>> {
        let enrollment = self.find(agent, repo_id, remote_id)?;
        let repo = self.repos.get(repo_id)?;
        Some(RemoteBinding { enrollment, repo })
    }
}

/// One enrollment joined with its repo.
#[derive(Debug, Clone, Copy)]
pub struct RemoteBinding<'a> {
    pub enrollment: &'a EnrollmentRecord,
    pub repo: &'a RepoRecord,
}

fn valid_identity(s: &str) -> bool {
    s.len() == IDENTITY_HEX_LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn new_identity() -> String {
    let mut buf = [0u8; IDENTITY_HEX_LEN / 2];
    // The crate is unix-only and has no RNG dependency; the OS source is enough for an id.
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("the OS random source is readable");
    hex::encode(buf)
}

/// Atomically replace `path` with `body` (temp file in the same directory, then rename).
pub fn write_atomic(path: &Path, body: &str, mode: u32) -> Result<(), BrokerError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let storage = |e: std::io::Error| BrokerError::Storage(e.to_string());
    let dir = path.parent().ok_or_else(|| bad("path"))?;
    std::fs::create_dir_all(dir).map_err(storage)?;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&tmp)
        .map_err(storage)?;
    f.write_all(body.as_bytes()).map_err(storage)?;
    f.sync_all().map_err(storage)?;
    std::fs::rename(&tmp, path).map_err(storage)
}

impl Enrollments {
    /// Write `enrollments.yaml` first, then the derived registry.
    pub fn save(&self, mur_home: &Path) -> Result<(), BrokerError> {
        use mur_common::git_push::{enrollments_path, registry_path};
        self.validate()?;
        let body =
            serde_yaml_ng::to_string(self).map_err(|e| BrokerError::Storage(e.to_string()))?;
        write_atomic(&enrollments_path(mur_home), &body, ENROLLMENTS_FILE_MODE)?;
        write_atomic(
            &registry_path(mur_home),
            &self.registry_yaml()?,
            REGISTRY_FILE_MODE,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(agent: &str) -> EnrollRequest {
        EnrollRequest {
            agent: agent.into(),
            repo_id: "mur".into(),
            remote_id: "origin".into(),
            endpoint: Endpoint::parse("file:///srv/remote.git").unwrap(),
            ref_prefix: format!("{ALLOWED_REF_PREFIX}{agent}/"),
            creation_base_refs: vec!["refs/heads/main".into()],
            credential: None,
            new_repo: Some(NewRepo {
                path: "/work/mur".into(),
                object_format: ObjectFormat::Sha1,
            }),
        }
    }
    fn fresh() -> Enrollments {
        Enrollments::empty(100)
    }

    #[test]
    fn enroll_creates_then_is_idempotent_then_updates_with_a_new_epoch() {
        let mut e = fresh();
        assert_eq!(e.enroll(req("alice")).unwrap(), EnrollOutcome::Created);
        let first = e.find("alice", "mur", "origin").unwrap().epoch;
        assert_eq!(e.enroll(req("alice")).unwrap(), EnrollOutcome::Unchanged);
        assert_eq!(e.find("alice", "mur", "origin").unwrap().epoch, first);
        let mut changed = req("alice");
        changed.creation_base_refs.clear();
        assert_eq!(e.enroll(changed).unwrap(), EnrollOutcome::Updated);
        assert!(e.find("alice", "mur", "origin").unwrap().epoch > first);
    }

    #[test]
    fn base_ref_order_and_repeats_do_not_count_as_a_change() {
        let mut e = fresh();
        let mut r = req("alice");
        r.creation_base_refs = vec!["refs/heads/b".into(), "refs/heads/a".into()];
        e.enroll(r.clone()).unwrap();
        r.creation_base_refs = vec![
            "refs/heads/a".into(),
            "refs/heads/b".into(),
            "refs/heads/a".into(),
        ];
        assert_eq!(e.enroll(r).unwrap(), EnrollOutcome::Unchanged);
    }

    #[test]
    fn a_revoked_enrollment_never_gets_its_epoch_back() {
        let mut e = fresh();
        e.enroll(req("alice")).unwrap();
        let old = e.find("alice", "mur", "origin").unwrap().epoch;
        assert!(e.revoke("alice", "mur", "origin"));
        assert!(e.repos.is_empty(), "last enrollment takes the repo with it");
        e.enroll(req("alice")).unwrap();
        assert!(e.find("alice", "mur", "origin").unwrap().epoch > old);
    }

    #[test]
    fn a_different_repo_for_a_known_id_is_refused_and_changes_nothing() {
        let mut e = fresh();
        e.enroll(req("alice")).unwrap();
        let before = e.clone();
        let mut r = req("bob");
        r.new_repo = Some(NewRepo {
            path: "/elsewhere".into(),
            object_format: ObjectFormat::Sha1,
        });
        assert!(e.enroll(r).is_err());
        assert_eq!(e, before);
    }

    #[test]
    fn a_new_repo_id_needs_a_path() {
        let mut r = req("alice");
        r.new_repo = None;
        assert!(fresh().enroll(r).is_err());
    }

    #[test]
    fn moving_a_repo_mints_a_new_identity_for_every_agent() {
        let mut e = fresh();
        e.enroll(req("alice")).unwrap();
        let mut b = req("bob");
        b.new_repo = None;
        e.enroll(b).unwrap();
        let id = e.repos["mur"].identity.clone();
        e.move_repo(
            "mur",
            NewRepo {
                path: "/new".into(),
                object_format: ObjectFormat::Sha1,
            },
        )
        .unwrap();
        assert_ne!(e.repos["mur"].identity, id);
        assert_eq!(
            e.binding("bob", "mur", "origin").unwrap().repo.path,
            Path::new("/new")
        );
    }

    #[test]
    fn registry_is_derived_with_sorted_distinct_remotes() {
        let mut e = fresh();
        e.enroll(req("alice")).unwrap();
        let mut b = req("bob");
        b.new_repo = None;
        e.enroll(b).unwrap();
        let mut o = req("alice");
        o.remote_id = "backup".into();
        o.new_repo = None;
        e.enroll(o).unwrap();
        let y = e.registry_yaml().unwrap();
        assert_eq!(
            y,
            "repos:\n  mur:\n    path: /work/mur\n    remotes:\n    - backup\n    - origin\n"
        );
    }

    #[test]
    fn a_wide_ref_prefix_is_refused_at_enroll() {
        let mut r = req("alice");
        r.ref_prefix = "refs/heads/".into();
        assert!(fresh().enroll(r).is_err());
    }

    #[test]
    fn load_fails_closed_on_any_bad_record() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("enrollments.yaml");
        let mut e = fresh();
        e.enroll(req("alice")).unwrap();
        let good = serde_yaml_ng::to_string(&e).unwrap();
        std::fs::write(&p, &good).unwrap();
        assert_eq!(Enrollments::load(&p).unwrap().unwrap(), e);
        for (from, to) in [
            ("file:///srv/remote.git", "ssh://h/r.git"),
            ("repo_id: mur", "repo_id: ghost"),
            (
                "ref_prefix: refs/heads/agent/alice/",
                "ref_prefix: refs/heads/",
            ),
            ("epoch: 100", "epoch: 0"),
        ] {
            assert!(good.contains(from), "{from}");
            std::fs::write(&p, good.replace(from, to)).unwrap();
            assert!(Enrollments::load(&p).is_err(), "{to}");
        }
        std::fs::write(&p, format!("{good}surprise: 1\n")).unwrap();
        assert!(Enrollments::load(&p).is_err(), "unknown field");
        std::fs::remove_file(&p).unwrap();
        assert_eq!(Enrollments::load(&p).unwrap(), None);
    }

    fn cred(path: &str, user: &str) -> Credential {
        Credential {
            secret_path: crate::credential::SecretPath::parse(path).unwrap(),
            username: crate::credential::Username::parse(user).unwrap(),
        }
    }
    fn https_req(agent: &str, c: Option<Credential>) -> EnrollRequest {
        EnrollRequest {
            endpoint: Endpoint::parse("https://git.example.com/o/r.git").unwrap(),
            credential: c,
            ..req(agent)
        }
    }

    #[test]
    fn a_record_without_a_credential_writes_no_credential_key_and_old_files_load() {
        let mut e = fresh();
        e.enroll(req("alice")).unwrap();
        let y = serde_yaml_ng::to_string(&e).unwrap();
        assert!(!y.contains("credential"), "{y}");
        // A file written before the field existed is exactly this text, and still loads.
        let back: Enrollments = serde_yaml_ng::from_str(&y).unwrap();
        assert_eq!(back.enrollments[0].credential, None);
        assert_eq!(back, e);
    }

    #[test]
    fn a_credential_survives_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("enrollments.yaml");
        let mut e = fresh();
        let c = cred("/h/.mur/secrets/gh", "x-access-token");
        e.enroll(https_req("alice", Some(c.clone()))).unwrap();
        std::fs::write(&p, serde_yaml_ng::to_string(&e).unwrap()).unwrap();
        let back = Enrollments::load(&p).unwrap().unwrap();
        assert_eq!(back.enrollments[0].credential, Some(c));
    }

    #[test]
    fn a_hand_edited_bad_credential_fails_the_load_closed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("enrollments.yaml");
        let mut e = fresh();
        e.enroll(https_req(
            "alice",
            Some(cred("/h/.mur/secrets/gh", "oauth2")),
        ))
        .unwrap();
        let good = serde_yaml_ng::to_string(&e).unwrap();
        for (from, to) in [
            ("/h/.mur/secrets/gh", "../gh"),
            ("username: oauth2", "username: 'a b'"),
            ("username: oauth2", "token: hunter2"),
        ] {
            assert!(good.contains(from), "{from}");
            std::fs::write(&p, good.replace(from, to)).unwrap();
            assert!(Enrollments::load(&p).is_err(), "{to}");
        }
    }

    #[test]
    fn changing_the_credential_is_an_update_with_a_new_epoch_and_the_same_one_is_not() {
        let mut e = fresh();
        let a = cred("/h/.mur/secrets/a", "u");
        assert_eq!(
            e.enroll(https_req("alice", Some(a.clone()))).unwrap(),
            EnrollOutcome::Created
        );
        let first = e.enrollments[0].epoch;
        assert_eq!(
            e.enroll(https_req("alice", Some(a))).unwrap(),
            EnrollOutcome::Unchanged
        );
        assert_eq!(e.enrollments[0].epoch, first);
        for changed in [
            Some(cred("/h/.mur/secrets/b", "u")),
            Some(cred("/h/.mur/secrets/a", "v")),
            None,
        ] {
            let before = e.enrollments[0].epoch;
            assert_eq!(
                e.enroll(https_req("alice", changed)).unwrap(),
                EnrollOutcome::Updated
            );
            assert!(e.enrollments[0].epoch > before);
            // Put a credential back so the next iteration changes it again.
            e.enroll(https_req("alice", Some(cred("/h/.mur/secrets/a", "u"))))
                .unwrap();
        }
    }

    #[test]
    fn save_writes_both_files_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut e = fresh();
        e.enroll(req("alice")).unwrap();
        e.save(dir.path()).unwrap();
        let ep = mur_common::git_push::enrollments_path(dir.path());
        let mode = std::fs::metadata(&ep).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, ENROLLMENTS_FILE_MODE);
        assert_eq!(
            std::fs::read_to_string(mur_common::git_push::registry_path(dir.path())).unwrap(),
            e.registry_yaml().unwrap()
        );
    }
}
