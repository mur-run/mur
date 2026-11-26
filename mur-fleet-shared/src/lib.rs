//! The shared file format of the fleet shared area (spec v3.26).
//!
//! One crate, used by both the agent runtime and the daemon, so the two sides
//! cannot disagree about a reference, an admission record, the index or a
//! refusal. It must not depend on `mur-core`: the runtime does not.

pub mod content;
pub mod payload;
pub mod refusal;
pub mod reply;
pub mod sign;

#[cfg(test)]
mod content_tests;
#[cfg(test)]
mod payload_tests;
#[cfg(test)]
mod refusal_tests;
#[cfg(test)]
mod reply_tests;
#[cfg(test)]
mod sign_tests;

pub use content::{ARCHIVE_MAGICS, ContentError, MAX_SUMMARY_BYTES, check_content, check_summary};
pub use payload::{
    AdmissionRecord, EntryState, Index, IndexEntry, REF_KIND, RefPayload, WIRE_VERSION, WireRef,
};
pub use refusal::{RefusalCode, SecretLabel};
pub use reply::{PublishOk, REPLY_UNPARSEABLE_TEXT, ReadOk, Reply, ReplyBody, ReplyUnparseable};
pub use sign::{REF_SIGN_DOMAIN, VerifyError, sign_ref, verify_ref};
