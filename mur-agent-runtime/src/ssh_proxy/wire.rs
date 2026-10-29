//! ssh-agent protocol codec (draft-miller-ssh-agent), only the messages the
//! proxy handles. Anything else decodes to [`WireError::UnknownType`] and is
//! refused by the caller as `malformed_request`.
//!
//! A frame is `uint32 length || byte type || payload`. Strings are
//! `uint32 length || bytes`.

use std::fmt;

/// Largest frame accepted in either direction. OpenSSH's own agent uses the
/// same bound (`AGENT_MAX_LEN`), so no legitimate client exceeds it.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

pub const SSH_AGENT_FAILURE: u8 = 5;
pub const SSH_AGENT_SUCCESS: u8 = 6;
pub const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
pub const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
pub const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
pub const SSH_AGENT_SIGN_RESPONSE: u8 = 14;
pub const SSH_AGENTC_EXTENSION: u8 = 27;

/// Extension name OpenSSH ≥ 8.9 sends to bind a connection to a host key.
pub const SESSION_BIND_EXTENSION: &str = "session-bind@openssh.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Frame body shorter than its declared content.
    Truncated,
    /// Declared length is zero or over [`MAX_FRAME_BYTES`].
    BadLength(usize),
    /// A message type the proxy does not handle in this direction.
    UnknownType(u8),
    /// Bytes left over after the message was fully parsed.
    TrailingBytes,
    /// A string that must be UTF-8 (extension name) was not.
    NotUtf8,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "truncated message"),
            Self::BadLength(n) => write!(f, "frame length {n} out of range"),
            Self::UnknownType(t) => write!(f, "unsupported message type {t}"),
            Self::TrailingBytes => write!(f, "trailing bytes after message"),
            Self::NotUtf8 => write!(f, "extension name is not UTF-8"),
        }
    }
}

impl std::error::Error for WireError {}

/// One key as listed by the upstream agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub key_blob: Vec<u8>,
    pub comment: Vec<u8>,
}

/// What a client may send the proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    RequestIdentities,
    SignRequest {
        key_blob: Vec<u8>,
        data: Vec<u8>,
        flags: u32,
    },
    SessionBind(SessionBind),
    /// Any extension other than `session-bind`. The proxy answers FAILURE,
    /// as an agent that does not implement it would.
    OtherExtension(String),
}

/// `session-bind@openssh.com` contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBind {
    pub host_key: Vec<u8>,
    pub session_id: Vec<u8>,
    pub signature: Vec<u8>,
    pub is_forwarding: bool,
}

/// What the upstream agent may answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Identities(Vec<Identity>),
    SignResponse(Vec<u8>),
    Success,
    Failure,
}

/// Cursor over a message body. Every read is bounds-checked.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.buf.len() < n {
            return Err(WireError::Truncated);
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn string(&mut self) -> Result<&'a [u8], WireError> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    pub(crate) fn finish(self) -> Result<(), WireError> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(WireError::TrailingBytes)
        }
    }

    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.buf)
    }
}

pub(crate) fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub(crate) fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    // Lengths are bounded by MAX_FRAME_BYTES before anything is encoded.
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s);
}

/// Prefix `body` (type byte + payload) with its length.
fn frame(body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 4);
    put_u32(&mut out, body.len() as u32);
    out.extend(body);
    out
}

/// Validate a declared frame length before reading the body.
pub fn check_len(declared: u32) -> Result<usize, WireError> {
    let n = declared as usize;
    if n == 0 || n > MAX_FRAME_BYTES {
        return Err(WireError::BadLength(n));
    }
    Ok(n)
}

/// Decode a client message body (without the length prefix).
pub fn decode_request(body: &[u8]) -> Result<Request, WireError> {
    let mut r = Reader::new(body);
    let req = match r.u8()? {
        SSH_AGENTC_REQUEST_IDENTITIES => Request::RequestIdentities,
        SSH_AGENTC_SIGN_REQUEST => Request::SignRequest {
            key_blob: r.string()?.to_vec(),
            data: r.string()?.to_vec(),
            flags: r.u32()?,
        },
        SSH_AGENTC_EXTENSION => {
            let name = std::str::from_utf8(r.string()?).map_err(|_| WireError::NotUtf8)?;
            if name != SESSION_BIND_EXTENSION {
                // Unknown extension contents are opaque; consume them.
                let _ = r.rest();
                return Ok(Request::OtherExtension(name.to_string()));
            }
            Request::SessionBind(SessionBind {
                host_key: r.string()?.to_vec(),
                session_id: r.string()?.to_vec(),
                signature: r.string()?.to_vec(),
                is_forwarding: r.u8()? != 0,
            })
        }
        other => return Err(WireError::UnknownType(other)),
    };
    r.finish()?;
    Ok(req)
}

/// Decode an upstream answer body (without the length prefix).
pub fn decode_answer(body: &[u8]) -> Result<Answer, WireError> {
    let mut r = Reader::new(body);
    let ans = match r.u8()? {
        SSH_AGENT_IDENTITIES_ANSWER => {
            let n = r.u32()? as usize;
            // Each identity takes at least 8 bytes; a count larger than the
            // body allows is malformed, and checking first bounds the alloc.
            if n > body.len() / 8 {
                return Err(WireError::Truncated);
            }
            let mut ids = Vec::with_capacity(n);
            for _ in 0..n {
                ids.push(Identity {
                    key_blob: r.string()?.to_vec(),
                    comment: r.string()?.to_vec(),
                });
            }
            Answer::Identities(ids)
        }
        SSH_AGENT_SIGN_RESPONSE => Answer::SignResponse(r.string()?.to_vec()),
        SSH_AGENT_SUCCESS => Answer::Success,
        SSH_AGENT_FAILURE => Answer::Failure,
        other => return Err(WireError::UnknownType(other)),
    };
    r.finish()?;
    Ok(ans)
}

impl Request {
    /// Encode as a full frame (used to forward to the upstream agent).
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        match self {
            Self::RequestIdentities => b.push(SSH_AGENTC_REQUEST_IDENTITIES),
            Self::SignRequest {
                key_blob,
                data,
                flags,
            } => {
                b.push(SSH_AGENTC_SIGN_REQUEST);
                put_string(&mut b, key_blob);
                put_string(&mut b, data);
                put_u32(&mut b, *flags);
            }
            Self::SessionBind(sb) => {
                b.push(SSH_AGENTC_EXTENSION);
                put_string(&mut b, SESSION_BIND_EXTENSION.as_bytes());
                put_string(&mut b, &sb.host_key);
                put_string(&mut b, &sb.session_id);
                put_string(&mut b, &sb.signature);
                b.push(u8::from(sb.is_forwarding));
            }
            Self::OtherExtension(name) => {
                b.push(SSH_AGENTC_EXTENSION);
                put_string(&mut b, name.as_bytes());
            }
        }
        frame(b)
    }
}

impl Answer {
    /// Encode as a full frame (sent back to the client).
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        match self {
            Self::Identities(ids) => {
                b.push(SSH_AGENT_IDENTITIES_ANSWER);
                put_u32(&mut b, ids.len() as u32);
                for id in ids {
                    put_string(&mut b, &id.key_blob);
                    put_string(&mut b, &id.comment);
                }
            }
            Self::SignResponse(sig) => {
                b.push(SSH_AGENT_SIGN_RESPONSE);
                put_string(&mut b, sig);
            }
            Self::Success => b.push(SSH_AGENT_SUCCESS),
            Self::Failure => b.push(SSH_AGENT_FAILURE),
        }
        frame(b)
    }
}

/// Read one frame body from `r`. `Ok(None)` is a clean EOF before any byte
/// of a new frame. The length is checked before the body is allocated.
pub async fn read_frame<R>(r: &mut R) -> std::io::Result<Option<Result<Vec<u8>, WireError>>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = match check_len(u32::from_be_bytes(len)) {
        Ok(n) => n,
        Err(e) => return Ok(Some(Err(e))),
    };
    let mut body = vec![0u8; n];
    r.read_exact(&mut body).await?;
    Ok(Some(Ok(body)))
}
