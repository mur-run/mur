//! Orchestration: one request from submission to a terminal state (design §4, §5).
//!
//! `submit_request` runs every check that touches untrusted bytes *before* a row is allowed to
//! reach `pending_approval`; any failure leaves no row, no notification and no repo on disk.
//! `on_approval` is the only path to a push, and it has no retry.

use crate::{
    action::ActionDocument,
    ancestry::judge,
    approval::{ApprovalProof, Clock, accept_approval, begin_execution},
    askpass::TokenAuth,
    binding::{Bound, EnrollmentSource, Mismatch, bind, build_action},
    credential::Credential,
    credential_role::{Phase, select},
    credential_source::CredentialSource,
    error::BrokerError,
    import::{ParserSpawn, import_pack},
    pending::{PendingStore, RequestKey, State, Submitted},
    policy::BrokerLimits,
    prefetch::{PrefetchGate, RemoteReader, run_prefetch},
    push::{RemoteAuth, push},
    repo::PrivateRepo,
    request_file::RequestFile,
};

/// Audit kind: a credential was handed to a pre-approval (read-only) prefetch. Kept apart from
/// the push, which only ever happens after approval, so the two can be told apart.
pub const AUDIT_PREFETCH_WITH_CREDENTIAL: &str = "prefetch_with_credential";
/// Audit kind: a human approval was accepted and spent on this request.
pub const AUDIT_APPROVAL_ACCEPTED: &str = "approval_accepted";
/// Audit kind: a credential was handed to the post-approval push.
pub const AUDIT_PUSH_WITH_CREDENTIAL: &str = "push_with_credential";
/// Audit kind prefix for the state a request ended in after approval, e.g. `ended:succeeded`.
pub const AUDIT_ENDED_PREFIX: &str = "ended:";

/// The audit kind for a request that ended in `state`.
pub fn audit_ended(state: State) -> String {
    format!("{AUDIT_ENDED_PREFIX}{}", state.as_str())
}
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Builds the prefetch reader for one remote URL. The URL comes from the enrollment record read
/// for that request, so a reader is never shared across remotes.
pub type ReaderFor = Box<dyn Fn(&str) -> Box<dyn RemoteReader> + Send + Sync>;

pub struct BrokerConfig {
    pub store_path: PathBuf,
    pub work_root: PathBuf,
    pub limits: BrokerLimits,
    pub git_bin: PathBuf,
    /// Read afresh at every decision; never cached.
    pub enrollments: Box<dyn EnrollmentSource>,
    pub reader: ReaderFor,
    pub spawn: Box<dyn ParserSpawn>,
    /// Used for remotes that carry no credential (`file://`, or an https remote with none).
    pub auth: Box<dyn RemoteAuth>,
    /// The binary git runs as `GIT_ASKPASS` for an https push with a credential. It must call
    /// [`crate::askpass::run_from_env`] first thing in `main` when `MUR_ASKPASS_FD` is set.
    pub askpass_bin: PathBuf,
    /// Where a credential's secret is read from. Only asked when the binding carries one.
    pub credentials: Box<dyn CredentialSource>,
    pub clock: Arc<dyn Clock>,
}

/// Test seam (`test-support` / `cfg(test)` only): called with the auth the push is about to run
/// with, the push-phase counterpart of what a `RemoteReader` sees in the prefetch.
#[cfg(any(test, feature = "test-support"))]
pub type PushObserver = Box<dyn Fn(&dyn RemoteAuth) + Send + Sync>;

pub struct Broker {
    cfg: BrokerConfig,
    store: PendingStore,
    gate: PrefetchGate,
    notifications: Mutex<Vec<String>>,
    #[cfg(any(test, feature = "test-support"))]
    push_observer: Option<PushObserver>,
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
            #[cfg(any(test, feature = "test-support"))]
            push_observer: None,
        })
    }

    /// Test seam: observe the auth handed to the push after approval.
    #[cfg(any(test, feature = "test-support"))]
    pub fn observe_push_auth(&mut self, f: PushObserver) {
        self.push_observer = Some(f);
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
        // The records are read here, at the decision, and what they say binds this request.
        let bound = match bind(&self.snapshot()?, doc) {
            Ok(b) => b,
            Err(Mismatch::NotEnrolled) => return Err(BrokerError::NotEnrolled),
            Err(Mismatch::Changed) => return Err(BrokerError::InvalidRequest("policy".into())),
        };
        let now = self.cfg.clock.now();
        // Caps and idempotency live inside `submit`'s transaction, ahead of any git work.
        if let Submitted::Existing(_) = self.store.submit(key, doc, &hash, now, &self.cfg.limits)? {
            return Ok(hash);
        }
        match self.prepare(key, doc, pack, &bound) {
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
                    let _ = self.store.audit(key, e.code(), now);
                }
                let _ = self.store.discard_validated(key);
                self.cleanup(key);
                Err(e)
            }
        }
    }

    /// The current records, or why there are none to act on.
    fn snapshot(&self) -> Result<crate::enrollment::Enrollments, BrokerError> {
        self.cfg
            .enrollments
            .current()?
            .ok_or(BrokerError::NotEnrolled)
    }

    /// Intake entry point: stamp a verified request with the current epoch and repo identity
    /// (the agent supplies neither), then submit it.
    pub fn submit_verified(&self, req: &RequestFile, pack: &Path) -> Result<String, BrokerError> {
        let doc = build_action(&self.snapshot()?, req).map_err(|_| BrokerError::NotEnrolled)?;
        let key = RequestKey {
            agent_id: req.agent.clone(),
            task_id: req.task_id.clone(),
            request_id: req.request_id.clone(),
        };
        self.submit_request(&key, &doc, pack)
    }

    /// Everything that touches remote or agent bytes. Returns the frozen control digest.
    fn prepare(
        &self,
        key: &RequestKey,
        doc: &ActionDocument,
        pack: &Path,
        bound: &Bound,
    ) -> Result<String, BrokerError> {
        let l = &self.cfg.limits;
        let reader = (self.cfg.reader)(&bound.remote_url);
        let repo = PrivateRepo::create(&self.dir_for(key), doc.object_format, &self.cfg.git_bin)?;
        // A private remote refuses an anonymous read, so the prefetch (which runs before any
        // approval) needs the enrollment's credential too. It is read-only: the same origin-bound
        // askpass wiring as the push, with redirects off. The secret lives only for this call and
        // is read again, fresh, at approval; nothing carries it from intake to execute.
        {
            // `file://` never prompts, so its credential is not even read before approval.
            let network = !bound
                .remote_url
                .starts_with(crate::constants::ENDPOINT_FILE_SCHEME);
            let selected = network.then(|| select(bound, Phase::Prefetch)).flatten();
            let secret = selected
                .map(|(cred, _)| self.open_secret(cred))
                .transpose()?;
            let token_auth = self.token_auth(bound, selected.map(|(c, _)| c), secret)?;
            if let (Some(_), Some((_, role))) = (&token_auth, selected) {
                let now = self.cfg.clock.now();
                let _ = self
                    .store
                    .audit_with_role(key, AUDIT_PREFETCH_WITH_CREDENTIAL, role, now);
            }
            let auth: &dyn RemoteAuth = match &token_auth {
                Some(t) => t,
                None => &*self.cfg.auth,
            };
            run_prefetch(&self.gate, &*reader, &repo, doc, &bound.policy, l, auth)?;
        }
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
        if row.state != State::PendingApproval
            || proof.request_id != key.request_id
            || proof.action_hash != row.action_hash
        {
            return Err(BrokerError::NotPending);
        }
        let doc: ActionDocument = serde_json::from_str(&row.doc_json).map_err(storage)?;
        // Read once, before the approval is spent. If the records cannot be read the row stays
        // pending and the approval unspent; if they no longer match, the request is void.
        // Execution uses this binding and does not look again.
        let bound = match self
            .snapshot()
            .and_then(|e| bind(&e, &doc).map_err(|_| BrokerError::PolicyChanged))
        {
            Ok(b) => b,
            Err(BrokerError::PolicyChanged | BrokerError::NotEnrolled) => {
                self.store
                    .transition(key, State::PendingApproval, State::PolicyChanged)?;
                self.audit_end(key, State::PolicyChanged);
                self.cleanup(key);
                return Err(BrokerError::PolicyChanged);
            }
            Err(e) => return Err(e),
        };
        // The secret is read after the binding holds and before the approval is spent, so a file
        // that cannot be used leaves the request pending and the same approval usable once it is
        // fixed. `?` here and on `accept_approval` drop the secret (`SecretString` zeroizes on
        // drop), so no early return leaves it behind.
        let secret = select(&bound, Phase::Push)
            .map(|(cred, _)| self.open_secret(cred))
            .transpose()?;
        let clock = &*self.cfg.clock;
        accept_approval(&self.store, key, &proof, clock)?;
        let _ = self.store.audit(key, AUDIT_APPROVAL_ACCEPTED, clock.now());
        if let Err(e) = begin_execution(&self.store, key, clock) {
            if e == BrokerError::ApprovalExpired {
                self.audit_end(key, State::ApprovalExpired);
            }
            self.cleanup(key);
            return Err(e);
        }
        let result = self.execute(key, &doc, &bound, secret);
        let (state, out) = match result {
            Ok(()) => (State::Succeeded, Ok(State::Succeeded)),
            Err(BrokerError::StaleOldSha) => (State::StaleOldSha, Err(BrokerError::StaleOldSha)),
            Err(e @ BrokerError::Rejected(_)) => (State::Rejected, Err(e)),
            Err(e @ BrokerError::OutcomeUnknown(_)) => (State::OutcomeUnknown, Err(e)),
            Err(e) => (State::Failed, Err(e)),
        };
        self.store.transition(key, State::Executing, state)?;
        self.audit_end(key, state);
        self.cleanup(key);
        out
    }

    /// Best effort, like every audit write here: a failed audit never changes an outcome that
    /// already happened.
    fn audit_end(&self, key: &RequestKey, state: State) {
        let _ = self
            .store
            .audit(key, &audit_ended(state), self.cfg.clock.now());
    }

    /// A verified human denial arrived. Ends the pending row as `denied` and drops its private
    /// repository. Anything that is not a pending row is `NotPending` and changes nothing.
    pub fn on_denial(&self, key: &RequestKey) -> Result<(), BrokerError> {
        self.store
            .transition(key, State::PendingApproval, State::Denied)?;
        self.audit_end(key, State::Denied);
        self.cleanup(key);
        Ok(())
    }

    fn execute(
        &self,
        key: &RequestKey,
        doc: &ActionDocument,
        bound: &Bound,
        // Taken by value so it is dropped (and zeroized) when this function returns, whatever
        // the outcome. `TokenAuth` copies it into a pipe per git command.
        secret: Option<SecretString>,
    ) -> Result<(), BrokerError> {
        let digest = self
            .store
            .frozen_digest(key)?
            .ok_or_else(|| storage("missing frozen digest"))?;
        let repo = PrivateRepo::open(&self.dir_for(key), &self.cfg.git_bin)?;
        let selected = select(bound, Phase::Push);
        let token_auth = self.token_auth(bound, selected.map(|(c, _)| c), secret)?;
        if let (Some(_), Some((_, role))) = (&token_auth, selected) {
            let _ = self.store.audit_with_role(
                key,
                AUDIT_PUSH_WITH_CREDENTIAL,
                role,
                self.cfg.clock.now(),
            );
        }
        let auth: &dyn RemoteAuth = match &token_auth {
            Some(t) => t,
            None => &*self.cfg.auth,
        };
        #[cfg(any(test, feature = "test-support"))]
        if let Some(observe) = &self.push_observer {
            observe(auth);
        }
        push(
            &repo,
            doc,
            &bound.remote_url,
            auth,
            &self.cfg.limits,
            &digest,
        )
    }
}

impl Broker {
    /// The secret `cred` points at, which the caller selected for its phase. `SecretString`
    /// zeroizes on drop.
    fn open_secret(&self, cred: &Credential) -> Result<SecretString, BrokerError> {
        self.cfg.credentials.open(&cred.secret_path)
    }

    /// The askpass wiring for `secret` on the bound remote, shared by prefetch and push so both
    /// are bound to the same origin by the same code.
    fn token_auth(
        &self,
        bound: &Bound,
        cred: Option<&Credential>,
        secret: Option<SecretString>,
    ) -> Result<Option<TokenAuth>, BrokerError> {
        let (Some(secret), Some(c)) = (secret, cred) else {
            return Ok(None);
        };
        let url = &bound.remote_url;
        let askpass = self.cfg.askpass_bin.clone();
        #[cfg(any(test, feature = "test-support"))]
        if url.starts_with(crate::constants::ENDPOINT_LOOPBACK_HTTP_PREFIX) {
            return Ok(TokenAuth::new_plain_http_for_test(
                secret,
                c.username.as_str(),
                url,
                askpass,
            ));
        }
        let built = TokenAuth::new(secret, c.username.as_str(), url, askpass);
        // `file://` has no password prompt, so a credential on it is simply unused. For an https
        // remote `None` cannot happen (enrollment refuses userinfo, `open` caps the size); if it
        // does, going on with no credential would only fail later and less clearly, so stop here.
        if built.is_none() && url.starts_with(crate::constants::ENDPOINT_HTTPS_SCHEME) {
            return Err(BrokerError::CredentialUnreadable("credential_unusable"));
        }
        Ok(built)
    }
}
