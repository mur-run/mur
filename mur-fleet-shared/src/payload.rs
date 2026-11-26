//! The data types of the store layout (spec v3.2): reference payload and wire
//! form, admission record, index. Units: `size` is bytes, `admitted_at` is
//! unix seconds as an integer, `seq` is a `u64` of at least 1.

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Wire version of every signed or exchanged structure in this crate.
pub const WIRE_VERSION: u32 = 1;

/// The `kind` field inside a signed reference payload. With the signing
/// domain prefix it keeps a channel event signature from passing as a
/// reference, and the reverse.
pub const REF_KIND: &str = "fleet_shared_ref";

/// Length of a request id, in lowercase hex characters (spec Transport).
pub const REQUEST_ID_LEN: usize = 32;

/// `seq` must be at least 1: 0 is "never used".
pub(crate) fn de_seq<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    match u64::deserialize(d)? {
        0 => Err(D::Error::custom("seq must be at least 1")),
        n => Ok(n),
    }
}

/// True iff `id` is [`REQUEST_ID_LEN`] lowercase hex characters.
pub fn valid_request_id(id: &str) -> bool {
    id.len() == REQUEST_ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn de_request_id<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let id = String::deserialize(d)?;
    if valid_request_id(&id) {
        Ok(id)
    } else {
        Err(D::Error::custom(
            "request_id must be 32 lowercase hex characters",
        ))
    }
}

/// What a reference signature covers. Serialized with `v` and `kind` added, so
/// the signed bytes carry both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RefPayloadRepr", into = "RefPayloadRepr")]
pub struct RefPayload {
    pub fleet: String,
    pub run: String,
    /// The signer, by definition: verifiers resolve the key from this member.
    pub member: String,
    pub name: String,
    pub seq: u64,
    pub sha256: String,
    pub size: u64,
    pub admitted_at: i64,
    /// Id of the publish request that produced the admission.
    pub request_id: String,
    pub key_version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefPayloadRepr {
    v: u32,
    kind: String,
    fleet: String,
    run: String,
    member: String,
    name: String,
    #[serde(deserialize_with = "de_seq")]
    seq: u64,
    sha256: String,
    size: u64,
    admitted_at: i64,
    #[serde(deserialize_with = "de_request_id")]
    request_id: String,
    key_version: u32,
}

impl From<RefPayload> for RefPayloadRepr {
    fn from(p: RefPayload) -> Self {
        Self {
            v: WIRE_VERSION,
            kind: REF_KIND.to_owned(),
            fleet: p.fleet,
            run: p.run,
            member: p.member,
            name: p.name,
            seq: p.seq,
            sha256: p.sha256,
            size: p.size,
            admitted_at: p.admitted_at,
            request_id: p.request_id,
            key_version: p.key_version,
        }
    }
}

impl TryFrom<RefPayloadRepr> for RefPayload {
    type Error = String;

    fn try_from(r: RefPayloadRepr) -> Result<Self, String> {
        if r.v != WIRE_VERSION {
            return Err(format!("unsupported reference version {}", r.v));
        }
        if r.kind != REF_KIND {
            return Err("not a fleet shared reference".to_owned());
        }
        Ok(Self {
            fleet: r.fleet,
            run: r.run,
            member: r.member,
            name: r.name,
            seq: r.seq,
            sha256: r.sha256,
            size: r.size,
            admitted_at: r.admitted_at,
            request_id: r.request_id,
            key_version: r.key_version,
        })
    }
}

/// A reference as it travels: `{"ref": <payload>, "sig": "<signature>"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRef {
    #[serde(rename = "ref")]
    pub payload: RefPayload,
    /// Multibase Ed25519 signature over [`crate::sign::REF_SIGN_DOMAIN`] plus
    /// the canonical JSON of `payload`.
    pub sig: String,
}

/// `admissions/<name>/<seq>.json`: one admission per (name, seq).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionRecord {
    pub fleet: String,
    pub run: String,
    pub member: String,
    pub name: String,
    #[serde(deserialize_with = "de_seq")]
    pub seq: u64,
    pub sha256: String,
    pub size: u64,
    /// The daemon's wall clock, unix seconds.
    pub admitted_at: i64,
    #[serde(deserialize_with = "de_request_id")]
    pub request_id: String,
}

/// Whether an index entry names readable content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryState {
    Live,
    /// Kept after expiry so `seq` never restarts.
    Tombstone,
}

/// One name in `index.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    #[serde(deserialize_with = "de_seq")]
    pub seq: u64,
    pub sha256: String,
    pub state: EntryState,
}

/// `index.json`: `name → entry`. The single commit point of the store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Index(pub BTreeMap<String, IndexEntry>);
