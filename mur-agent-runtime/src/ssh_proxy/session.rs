//! One client connection (plan Task 2.3, per-connection part).
//!
//! Independent of where the proxy runs (spec §"Proxy process placement",
//! open): the upstream is behind [`Upstream`]. A sign request is forwarded
//! only after the binding rules, the policy, and the approval gate
//! ([`SignGate`]) all pass; there is no other path to a signature.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use super::gate::{GateOutcome, SignAsk, SignGate};
use super::keys::{self, fingerprint};
use super::policy::Policy;
use super::refusal::{Refusal, RefusalCode, RefusalSink};
use super::userauth;
use super::wire::{self, Answer, Request, SessionBind};

/// Most `session-bind`s one connection may carry. OpenSSH's agent uses the
/// same bound; past it the connection is refused.
pub const MAX_SESSION_BINDS: usize = 16;

/// The real ssh-agent, however the proxy reaches it.
pub(crate) trait Upstream {
    /// Send one framed request, return the answer body (no length prefix).
    async fn roundtrip(&self, frame: Vec<u8>) -> std::io::Result<Vec<u8>>;
}

/// What this connection has proven about its destination.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Bind {
    #[default]
    None,
    /// The most recent verified bind (spec "Binding sign requests", rule 1).
    Verified(Verified),
    /// A bind failed to decode or verify (or exceeded the cap). Sticky: a later valid
    /// bind does not clear it, so a client cannot retry its way past.
    Poisoned(String),
}

/// What a verified `session-bind` proved. Each new verified bind replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Verified {
    host_key_fp: String,
    host_key: Vec<u8>,
    session_id: Vec<u8>,
    is_forwarding: bool,
}

pub(crate) struct Session<'a, U> {
    policy: &'a Policy,
    upstream: &'a U,
    sink: &'a RefusalSink,
    gate: &'a dyn SignGate,
    bind: Bind,
    binds_seen: usize,
}

impl<'a, U: Upstream> Session<'a, U> {
    pub(crate) fn new(
        policy: &'a Policy,
        upstream: &'a U,
        sink: &'a RefusalSink,
        gate: &'a dyn SignGate,
    ) -> Self {
        Self {
            policy,
            upstream,
            sink,
            gate,
            bind: Bind::None,
            binds_seen: 0,
        }
    }

    /// Serve until the client closes. An oversize or unparseable length ends
    /// the connection after answering FAILURE: the stream cannot be resynced.
    pub(crate) async fn serve<S>(mut self, mut stream: S) -> std::io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        loop {
            let body = match wire::read_frame(&mut stream).await? {
                None => return Ok(()),
                Some(Ok(body)) => body,
                Some(Err(e)) => {
                    self.refuse(Refusal::new(RefusalCode::MalformedRequest).detail(e.to_string()));
                    stream.write_all(&Answer::Failure.encode()).await?;
                    return Ok(());
                }
            };
            let answer = self.handle(&body).await;
            stream.write_all(&answer.encode()).await?;
        }
    }

    /// Answer one message body. Pure apart from the upstream and the sink.
    pub(crate) async fn handle(&mut self, body: &[u8]) -> Answer {
        let req = match wire::decode_request(body) {
            Ok(r) => r,
            Err(e) => {
                // An undecodable extension may be a session-bind attempt. Even
                // a truncated name must not preserve an earlier verified bind
                // or permit recovery via a later valid bind. Well-formed
                // unknown extensions still follow OtherExtension below.
                if body.first() == Some(&wire::SSH_AGENTC_EXTENSION)
                    && !matches!(self.bind, Bind::Poisoned(_))
                {
                    self.bind = Bind::Poisoned(format!("malformed extension: {e}"));
                }
                self.refuse(Refusal::new(RefusalCode::MalformedRequest).detail(e.to_string()));
                return Answer::Failure;
            }
        };
        match req {
            Request::RequestIdentities => self.list().await,
            Request::SessionBind(sb) => self.session_bind(&sb),
            Request::SignRequest {
                key_blob,
                data,
                flags,
            } => self.sign(key_blob, data, flags).await,
            // Not an error: an agent without the extension answers FAILURE.
            Request::OtherExtension(_) => Answer::Failure,
        }
    }

    async fn list(&self) -> Answer {
        if let Policy::RefuseAll(_) = self.policy {
            return Answer::Identities(Vec::new());
        }
        let body = match self
            .upstream
            .roundtrip(Request::RequestIdentities.encode())
            .await
        {
            Ok(b) => b,
            Err(e) => {
                self.refuse(Refusal::new(RefusalCode::UpstreamUnavailable).detail(e.to_string()));
                return Answer::Failure;
            }
        };
        match wire::decode_answer(&body) {
            Ok(Answer::Identities(ids)) => Answer::Identities(
                ids.into_iter()
                    .filter(|id| self.policy.lists(&fingerprint(&id.key_blob)))
                    .collect(),
            ),
            Ok(_) | Err(_) => {
                self.refuse(
                    Refusal::new(RefusalCode::UpstreamUnavailable)
                        .detail("upstream answered REQUEST_IDENTITIES with something else"),
                );
                Answer::Failure
            }
        }
    }

    /// Verify the bind here; it is not forwarded (spec §"`session-bind`
    /// verification"). The refusal, if any, is recorded at sign time, where
    /// the key is known.
    fn session_bind(&mut self, sb: &SessionBind) -> Answer {
        self.binds_seen += 1;
        let host_fp = fingerprint(&sb.host_key);
        if self.binds_seen > MAX_SESSION_BINDS {
            self.bind = Bind::Poisoned(format!("more than {MAX_SESSION_BINDS} session-binds"));
            return Answer::Failure;
        }
        if matches!(self.bind, Bind::Poisoned(_)) {
            return Answer::Failure;
        }
        match keys::verify_session_bind(&sb.host_key, &sb.session_id, &sb.signature) {
            Ok(()) => {
                self.bind = Bind::Verified(Verified {
                    host_key_fp: host_fp,
                    host_key: sb.host_key.clone(),
                    session_id: sb.session_id.clone(),
                    is_forwarding: sb.is_forwarding,
                });
                Answer::Success
            }
            Err(e) => {
                self.bind = Bind::Poisoned(format!("host key {host_fp}: {e}"));
                Answer::Failure
            }
        }
    }

    /// Binding rules, then policy, then the approval gate; only an approved
    /// request is forwarded. Every refusal is recorded with the most
    /// specific code and answered FAILURE.
    async fn sign(&self, key_blob: Vec<u8>, data: Vec<u8>, flags: u32) -> Answer {
        let key_fp = fingerprint(&key_blob);
        let host = match &self.bind {
            Bind::Verified(v) => {
                // Rule 6: after bind presence, before the key/host lookup,
                // so a mismatch never reaches an approval prompt.
                if let Err((code, detail)) = check_binding(v, &key_blob, &data) {
                    let r = Refusal::new(code)
                        .key(key_fp.clone(), self.policy.label(&key_fp))
                        .host(v.host_key_fp.clone())
                        .detail(detail);
                    self.refuse(r);
                    return Answer::Failure;
                }
                Some(v.host_key_fp.as_str())
            }
            Bind::None | Bind::Poisoned(_) => None,
        };
        let allowed = match self.policy.check_sign(&key_fp, host) {
            Ok(a) => a,
            Err(r) if r.code == RefusalCode::NoSessionBind => {
                self.refuse(match &self.bind {
                    Bind::Poisoned(why) => Refusal {
                        code: RefusalCode::SessionBindUnverified,
                        ..r
                    }
                    .detail(why.clone()),
                    _ => r,
                });
                return Answer::Failure;
            }
            Err(r) => {
                self.refuse(r);
                return Answer::Failure;
            }
        };
        let ask = SignAsk {
            key_fp: allowed.key_fp,
            key_label: allowed.key_label,
            host_key_fp: allowed.host_key_fp,
            host_label: allowed.host_label,
        };
        let refused = |code| {
            Refusal::new(code)
                .key(ask.key_fp.clone(), ask.key_label.clone())
                .host(ask.host_key_fp.clone())
        };
        match self.gate.decide(&ask).await {
            GateOutcome::Allow => {}
            GateOutcome::Deny => {
                self.refuse(refused(RefusalCode::HitlDenied));
                return Answer::Failure;
            }
            GateOutcome::Unavailable(why) => {
                self.refuse(refused(RefusalCode::HitlUnavailable).detail(why));
                return Answer::Failure;
            }
            GateOutcome::Deferred => {
                self.refuse(refused(RefusalCode::HitlDeferred));
                return Answer::Failure;
            }
        }
        let frame = Request::SignRequest {
            key_blob,
            data,
            flags,
        }
        .encode();
        let why = match self.upstream.roundtrip(frame).await {
            Err(e) => e.to_string(),
            Ok(body) => match wire::decode_answer(&body) {
                Ok(a @ Answer::SignResponse(_)) => return a,
                Ok(Answer::Failure) => "the upstream ssh-agent refused to sign".into(),
                Ok(_) => "the upstream answered SIGN_REQUEST with something else".into(),
                Err(e) => e.to_string(),
            },
        };
        self.refuse(refused(RefusalCode::UpstreamUnavailable).detail(why));
        Answer::Failure
    }

    fn refuse(&self, r: Refusal) {
        self.sink.record(&r);
    }
}

/// Spec "Binding sign requests to the verified session-bind", rules 2-5.
/// The detail names short hashes only, never the session ID or user (rule 7).
fn check_binding(v: &Verified, key_blob: &[u8], data: &[u8]) -> Result<(), (RefusalCode, String)> {
    if v.is_forwarding {
        return Err((
            RefusalCode::ForwardingNotSupported,
            "bind has is_forwarding set".into(),
        ));
    }
    let req =
        userauth::parse(data).map_err(|e| (RefusalCode::SignDataNotUserauth, e.to_string()))?;
    if !bool::from(req.session_id.ct_eq(&v.session_id)) {
        return Err((
            RefusalCode::SessionIdMismatch,
            format!(
                "bound session {}, signature for session {}",
                short_hash(&v.session_id),
                short_hash(req.session_id)
            ),
        ));
    }
    if req.key_blob != key_blob {
        return Err((
            RefusalCode::SignRequestInconsistent,
            format!("login request names key {}", fingerprint(req.key_blob)),
        ));
    }
    if let Some(h) = req.host_key
        && h != v.host_key.as_slice()
    {
        return Err((
            RefusalCode::SignRequestInconsistent,
            format!("login request names host key {}", fingerprint(h)),
        ));
    }
    Ok(())
}

/// First 8 hex digits of SHA-256: enough to correlate log lines, not enough
/// to recover the value.
fn short_hash(b: &[u8]) -> String {
    Sha256::digest(b)[..4]
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}
