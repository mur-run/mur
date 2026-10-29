//! One client connection (plan Task 2.3, per-connection part).
//!
//! Independent of where the proxy runs (spec §"Proxy process placement",
//! open): the upstream is behind [`Upstream`]. In Phase 2 no sign request is
//! ever forwarded — there is no code path that produces a signature.

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use super::keys::{self, fingerprint};
use super::policy::Policy;
use super::refusal::{Refusal, RefusalCode, RefusalSink};
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
    /// Host-key fingerprint of the most recent verified bind.
    Verified(String),
    /// A bind failed to verify (or exceeded the cap). Sticky: a later valid
    /// bind does not clear it, so a client cannot retry its way past.
    Poisoned(String),
}

pub(crate) struct Session<'a, U> {
    policy: &'a Policy,
    upstream: &'a U,
    sink: &'a RefusalSink,
    bind: Bind,
    binds_seen: usize,
}

impl<'a, U: Upstream> Session<'a, U> {
    pub(crate) fn new(policy: &'a Policy, upstream: &'a U, sink: &'a RefusalSink) -> Self {
        Self {
            policy,
            upstream,
            sink,
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
                self.refuse(Refusal::new(RefusalCode::MalformedRequest).detail(e.to_string()));
                return Answer::Failure;
            }
        };
        match req {
            Request::RequestIdentities => self.list().await,
            Request::SessionBind(sb) => self.session_bind(&sb),
            Request::SignRequest { key_blob, .. } => {
                self.sign(&key_blob);
                Answer::Failure
            }
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
                self.bind = Bind::Verified(host_fp);
                Answer::Success
            }
            Err(e) => {
                self.bind = Bind::Poisoned(format!("host key {host_fp}: {e}"));
                Answer::Failure
            }
        }
    }

    /// Run every policy check so the refusal is the most specific one, then
    /// refuse anyway: Phase 2 has no HITL, so nothing is signed.
    fn sign(&self, key_blob: &[u8]) {
        let key_fp = fingerprint(key_blob);
        let host = match &self.bind {
            Bind::Verified(h) => Some(h.as_str()),
            Bind::None | Bind::Poisoned(_) => None,
        };
        let refusal = match self.policy.check_sign(&key_fp, host) {
            Err(r) if r.code == RefusalCode::NoSessionBind => match &self.bind {
                Bind::Poisoned(why) => Refusal {
                    code: RefusalCode::SessionBindUnverified,
                    ..r
                }
                .detail(why.clone()),
                _ => r,
            },
            Err(r) => r,
            Ok(a) => Refusal::new(RefusalCode::HitlNotWired)
                .key(a.key_fp, a.key_label)
                .host(a.host_key_fp),
        };
        self.refuse(refusal);
    }

    fn refuse(&self, r: Refusal) {
        self.sink.record(&r);
    }
}
