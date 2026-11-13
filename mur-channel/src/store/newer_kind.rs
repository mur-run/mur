//! Lines written by a newer MUR (#2287): a well-formed event envelope whose
//! `kind` this binary does not know.
//!
//! Not knowing the kind is no reason to trust the rest of the line. The
//! envelope carries every field the signature and the hash chain cover — the
//! kind travels as the raw string the writer signed — so such a line is
//! verified exactly like a known one; only folding it is left to a binary
//! that knows the kind.

use chrono::{DateTime, Utc};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

/// The [`ChannelEvent`] envelope with `kind` as its raw wire string. Same
/// required fields (`seq`, `ts`, `actor`, `kind`) and the same defaults, so
/// a line that is missing one is corrupt here too.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct NewerKindEvent {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub actor: ChannelActor,
    pub kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub sig: Option<String>,
    #[serde(default)]
    pub key_version: Option<u32>,
    #[serde(default)]
    pub prev_hash: Option<String>,
}

impl NewerKindEvent {
    pub(crate) fn signed_fields(&self) -> crate::sign::SignedFields<'_> {
        crate::sign::SignedFields {
            actor: &self.actor,
            kind: serde_json::Value::String(self.kind.clone()),
            payload: &self.payload,
            idempotency_key: self.idempotency_key.as_deref(),
            sig: self.sig.as_deref(),
            prev_hash: self.prev_hash.as_deref(),
        }
    }
}

/// One non-blank log line, as this binary reads it.
pub(crate) enum Line {
    Event(ChannelEvent),
    /// A healthy envelope with a kind this binary does not know.
    NewerKind(NewerKindEvent),
    /// Anything else: not JSON, truncated, a missing or mistyped required
    /// field, or a known kind whose body does not parse.
    Corrupt,
}

pub(crate) fn parse_line(line: &str) -> Line {
    if let Ok(ev) = serde_json::from_str::<ChannelEvent>(line) {
        return Line::Event(ev);
    }
    match serde_json::from_str::<NewerKindEvent>(line) {
        Ok(ev) if !is_known_kind(&ev.kind) => Line::NewerKind(ev),
        _ => Line::Corrupt,
    }
}

fn is_known_kind(kind: &str) -> bool {
    serde_json::from_value::<EventKind>(serde_json::Value::String(kind.to_owned())).is_ok()
}
