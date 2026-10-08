//! Orchestration: one request from submission to a terminal state (design §4, §5).
//!
//! `submit_request` runs every check that touches untrusted bytes *before* a row is allowed to
//! reach `pending_approval`; any failure leaves no row, no notification and no repo on disk.
//! `on_approval` is the only path to a push, and it has no retry.

use crate::{
    action::ActionDocument,
    ancestry::judge,
    approval::{ApprovalProof, Clock, accept_approval, begin_execution},
    error::BrokerError,
    import::{ParserSpawn, import_pack},
    pending::{PendingStore, RequestKey, State, Submitted},
    policy::{BrokerLimits, RemotePolicy},
    prefetch::{PrefetchGate, RemoteReader, run_prefetch},
    push::{RemoteAuth, push},
    repo::PrivateRepo,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub struct BrokerConfig {
    pub store_path: PathBuf,
    pub work_root: PathBuf,
    pub limits: BrokerLimits,
    pub git_bin: PathBuf,
    pub policy: RemotePolicy,
    pub remote_url: String,
    pub reader: Box<dyn RemoteReader>,
    pub spawn: Box<dyn ParserSpawn>,
    pub auth: Box<dyn RemoteAuth>,
    pub clock: Arc<dyn Clock>,
}

pub struct Broker {
    cfg: BrokerConfig,
    store: PendingStore,
    gate: PrefetchGate,
    notifications: Mutex<Vec<String>>,
}

fn storage(e: impl std::fmt::Display) -> BrokerError {
    BrokerError::Storage(e.to_string())
}

impl Broker {
    /// Opens the store and settles whatever a previous process left half-done (see
    /// `PendingStore::recover`).
    pub fn new(cfg: BrokerConfig) -> Result<Broker, BrokerError> {
        fs::create_dir_all(&cfg.work_root).map_err(storage)?;
        let store = PendingStore::open(&cfg.store_path)?;
        store.recover()?;
        let gate = PrefetchGate::new(cfg.limits.max_concurrent_prefetch);
        Ok(Broker {
            cfg,
            store,
            gate,
            notifications: Mutex::new(Vec::new()),
        })
    }

    pub fn store(&self) -> &PendingStore {
        &self.store
    }

    /// Request ids that became pending since the last call.
    pub fn take_notifications(&self) -> Vec<String> {
        let mut n = self.notifications.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *n)
    }

    /// A directory name that is a function of the key alone, so path-like ids cannot escape.
    fn dir_for(&self, key: &RequestKey) -> PathBuf {
        let mut h = Sha256::new();
        for part in [&key.agent_id, &key.task_id, &key.request_id] {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part.as_bytes());
        }
        self.cfg.work_root.join(hex::encode(h.finalize()))
    }

    fn cleanup(&self, key: &RequestKey) {
        let dir = self.dir_for(key);
        if let Ok(repo) = PrivateRepo::open(&dir, &self.cfg.git_bin) {
            repo.destroy();
        }
        let _ = fs::remove_dir_all(&dir);
    }
}

impl Broker {
    /// Validate, bound, import and judge a request. On success the row is `pending_approval`
    /// with its private repo frozen on disk. On any error before that there is no row, no
    /// notification and no repo.
    pub fn submit_request(
        &self,
        key: &RequestKey,
        doc: &ActionDocument,
        pack: &Path,
    ) -> Result<String, BrokerError> {
        let hash = doc.action_hash()?;
        let p = &self.cfg.policy;
        if doc.ref_policy_digest != p.digest()
            || doc.remote_id != p.remote_id
            || doc.canonical_remote_endpoint != p.canonical_remote_endpoint
        {
            return Err(BrokerError::InvalidRequest("policy".into()));
        }
        let now = self.cfg.clock.now();
        // Caps and idempotency live inside `submit`'s transaction, ahead of any git work.
        if let Submitted::Existing(_) = self.store.submit(key, doc, &hash, now, &self.cfg.limits)? {
            return Ok(hash);
        }
        match self.prepare(key, doc, pack) {
            Ok(digest) => {
                self.store.to_pending(key, &digest)?;
                self.notifications
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(key.request_id.clone());
                Ok(hash)
            }
            Err(e) => {
                if matches!(e, BrokerError::PrefetchRejected(_)) {
                    let _ = self.store.audit(&key.agent_id, e.code(), now);
                }
                let _ = self.store.discard_validated(key);
                self.cleanup(key);
                Err(e)
            }
        }
    }

    /// Everything that touches remote or agent bytes. Returns the frozen control digest.
    fn prepare(
        &self,
        key: &RequestKey,
        doc: &ActionDocument,
        pack: &Path,
    ) -> Result<String, BrokerError> {
        let l = &self.cfg.limits;
        let repo = PrivateRepo::create(&self.dir_for(key), doc.object_format, &self.cfg.git_bin)?;
        run_prefetch(
            &self.gate,
            &*self.cfg.reader,
            &repo,
            doc,
            &self.cfg.policy,
            l,
        )?;
        // Prefetch writes refs, so the control files are frozen only after it, and before the
        // agent's bytes reach the parser.
        let frozen = repo.freeze()?;
        import_pack(&repo, pack, doc, l, &*self.cfg.spawn)?;
        judge(&repo, doc, l)?;
        if repo.control_digest()? != frozen {
            return Err(BrokerError::AncestryUnprovable(
                "control files changed during import".into(),
            ));
        }
        Ok(frozen)
    }

    /// A verified human approval arrived. Accepts it, starts execution inside the window and
    /// pushes once. Returns the terminal state, or the error that ended the request.
    pub fn on_approval(
        &self,
        key: &RequestKey,
        proof: ApprovalProof,
    ) -> Result<State, BrokerError> {
        let row = self.store.get(key)?.ok_or(BrokerError::NotPending)?;
        let matches_row =
            proof.request_id == key.request_id && proof.action_hash == row.action_hash;
        if row.state == State::PendingApproval && matches_row {
            let doc: ActionDocument = serde_json::from_str(&row.doc_json).map_err(storage)?;
            if doc.ref_policy_digest != self.cfg.policy.digest() {
                self.store
                    .transition(key, State::PendingApproval, State::PolicyChanged)?;
                self.cleanup(key);
                return Err(BrokerError::PolicyChanged);
            }
        }
        let clock = &*self.cfg.clock;
        accept_approval(&self.store, key, &proof, clock)?;
        if let Err(e) = begin_execution(&self.store, key, clock) {
            self.cleanup(key);
            return Err(e);
        }
        let doc: ActionDocument = serde_json::from_str(&row.doc_json).map_err(storage)?;
        let result = self.execute(key, &doc);
        let (state, out) = match result {
            Ok(()) => (State::Succeeded, Ok(State::Succeeded)),
            Err(BrokerError::StaleOldSha) => (State::StaleOldSha, Err(BrokerError::StaleOldSha)),
            Err(e @ BrokerError::Rejected(_)) => (State::Rejected, Err(e)),
            Err(e @ BrokerError::OutcomeUnknown(_)) => (State::OutcomeUnknown, Err(e)),
            Err(e) => (State::Failed, Err(e)),
        };
        self.store.transition(key, State::Executing, state)?;
        self.cleanup(key);
        out
    }

    fn execute(&self, key: &RequestKey, doc: &ActionDocument) -> Result<(), BrokerError> {
        let digest = self
            .store
            .frozen_digest(key)?
            .ok_or_else(|| storage("missing frozen digest"))?;
        let repo = PrivateRepo::open(&self.dir_for(key), &self.cfg.git_bin)?;
        push(
            &repo,
            doc,
            &self.cfg.remote_url,
            &*self.cfg.auth,
            &self.cfg.limits,
            &digest,
        )
    }
}
