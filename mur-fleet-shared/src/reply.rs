//! The daemon's reply to a request (spec Transport). Unsigned; parsed as a
//! whole or not at all.

use crate::payload::{WIRE_VERSION, de_request_id, de_seq};
use crate::refusal::RefusalCode;
use serde::{Deserialize, Serialize};

/// The one sentence shown for a reply that does not parse as a whole: another
/// `v`, an unknown field, an unknown code or label, a malformed parameter or
/// invalid JSON. Nothing from the reply or the parser's error is shown.
pub const REPLY_UNPARSEABLE_TEXT: &str =
    "the daemon's reply was not understood; restart the daemon and the agent after an upgrade";

/// A reply that did not parse. Carries no detail on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", REPLY_UNPARSEABLE_TEXT)]
pub struct ReplyUnparseable;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishOk {
    pub seq: u64,
    pub sha256: String,
    pub size: u64,
    pub admitted_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadOk {
    pub content: String,
    pub size: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyBody {
    Publish(PublishOk),
    Read(ReadOk),
    Refused(RefusalCode),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub request_id: String,
    pub body: ReplyBody,
}

impl Reply {
    /// Parse a whole reply file. Any failure is [`ReplyUnparseable`].
    pub fn parse(bytes: &[u8]) -> Result<Reply, ReplyUnparseable> {
        let repr: ReplyRepr = serde_json::from_slice(bytes).map_err(|_| ReplyUnparseable)?;
        Reply::try_from(repr)
    }

    /// The reply file's bytes.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(&ReplyRepr::from(self.clone()))
            .expect("a reply is plain data and always serializes")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Ok,
    Refused,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishRepr {
    #[serde(deserialize_with = "de_seq")]
    seq: u64,
    sha256: String,
    size: u64,
    admitted_at: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRepr {
    content: String,
    size: u64,
    truncated: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefusedRepr {
    code: RefusalCode,
}

/// The reply file as JSON. There is no `message` field, and
/// `deny_unknown_fields` rejects a reply that has one.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplyRepr {
    v: u32,
    #[serde(deserialize_with = "de_request_id")]
    request_id: String,
    outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publish: Option<PublishRepr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read: Option<ReadRepr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refused: Option<RefusedRepr>,
}

impl TryFrom<ReplyRepr> for Reply {
    type Error = ReplyUnparseable;

    fn try_from(r: ReplyRepr) -> Result<Self, ReplyUnparseable> {
        if r.v != WIRE_VERSION {
            return Err(ReplyUnparseable);
        }
        // Exactly one body, and it must match `outcome`.
        let body = match (r.outcome, r.publish, r.read, r.refused) {
            (Outcome::Ok, Some(p), None, None) => ReplyBody::Publish(PublishOk {
                seq: p.seq,
                sha256: p.sha256,
                size: p.size,
                admitted_at: p.admitted_at,
            }),
            (Outcome::Ok, None, Some(rd), None) => ReplyBody::Read(ReadOk {
                content: rd.content,
                size: rd.size,
                truncated: rd.truncated,
            }),
            (Outcome::Refused, None, None, Some(f)) => ReplyBody::Refused(f.code),
            _ => return Err(ReplyUnparseable),
        };
        Ok(Reply {
            request_id: r.request_id,
            body,
        })
    }
}

impl From<Reply> for ReplyRepr {
    fn from(reply: Reply) -> Self {
        let mut repr = ReplyRepr {
            v: WIRE_VERSION,
            request_id: reply.request_id,
            outcome: Outcome::Ok,
            publish: None,
            read: None,
            refused: None,
        };
        match reply.body {
            ReplyBody::Publish(p) => {
                repr.publish = Some(PublishRepr {
                    seq: p.seq,
                    sha256: p.sha256,
                    size: p.size,
                    admitted_at: p.admitted_at,
                });
            }
            ReplyBody::Read(r) => {
                repr.read = Some(ReadRepr {
                    content: r.content,
                    size: r.size,
                    truncated: r.truncated,
                });
            }
            ReplyBody::Refused(code) => {
                repr.outcome = Outcome::Refused;
                repr.refused = Some(RefusedRepr { code });
            }
        }
        repr
    }
}
