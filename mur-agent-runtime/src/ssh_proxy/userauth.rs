//! Strict parser for the data an ssh client asks the agent to sign during
//! publickey authentication (spec "Binding sign requests to the verified
//! session-bind", rule 2). RFC 4252 section 7; OpenSSH `PROTOCOL` section 2.5
//! for the hostbound method.
//!
//! Anything that does not parse exactly — other services, other methods,
//! trailing bytes, SSHSIG blobs — is an error, and the caller refuses it.

use super::wire::{Reader, WireError};

/// `SSH_MSG_USERAUTH_REQUEST`.
const SSH_MSG_USERAUTH_REQUEST: u8 = 50;
/// The only service a publickey login is for.
const SERVICE: &[u8] = b"ssh-connection";
const METHOD_PUBLICKEY: &[u8] = b"publickey";
const METHOD_HOSTBOUND: &[u8] = b"publickey-hostbound-v00@openssh.com";
/// RFC 4251 boolean TRUE as OpenSSH encodes it: a real signature, not a query.
const TRUE: u8 = 1;

/// The fields of a userauth request the proxy checks.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UserauthRequest<'a> {
    pub session_id: &'a [u8],
    pub key_blob: &'a [u8],
    /// The server host key; `Some` only for the hostbound method.
    pub host_key: Option<&'a [u8]>,
}

/// Why the data is not a userauth request. Only logged, never shown with
/// field contents (spec rule 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotUserauth {
    Wire(WireError),
    MessageType(u8),
    Service,
    Method,
    NotASignature,
}

impl From<WireError> for NotUserauth {
    fn from(e: WireError) -> Self {
        Self::Wire(e)
    }
}

impl std::fmt::Display for NotUserauth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(e) => write!(f, "{e}"),
            Self::MessageType(t) => write!(f, "message type {t}, not USERAUTH_REQUEST"),
            Self::Service => write!(f, "service is not ssh-connection"),
            Self::Method => write!(f, "method is not publickey"),
            Self::NotASignature => write!(f, "boolean is not TRUE"),
        }
    }
}

pub(crate) fn parse(data: &[u8]) -> Result<UserauthRequest<'_>, NotUserauth> {
    let mut r = Reader::new(data);
    let session_id = r.string()?;
    let t = r.u8()?;
    if t != SSH_MSG_USERAUTH_REQUEST {
        return Err(NotUserauth::MessageType(t));
    }
    let _user = r.string()?;
    if r.string()? != SERVICE {
        return Err(NotUserauth::Service);
    }
    let hostbound = match r.string()? {
        METHOD_PUBLICKEY => false,
        METHOD_HOSTBOUND => true,
        _ => return Err(NotUserauth::Method),
    };
    if r.u8()? != TRUE {
        return Err(NotUserauth::NotASignature);
    }
    let _alg = r.string()?;
    let key_blob = r.string()?;
    let host_key = if hostbound { Some(r.string()?) } else { None };
    r.finish()?;
    Ok(UserauthRequest {
        session_id,
        key_blob,
        host_key,
    })
}
