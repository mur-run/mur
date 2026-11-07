//! A channel conversation as the agent runtime reads it (§3.4): the channel
//! log is the only source of truth, read under ONE shared lock, and every
//! damage state is refused with a named [`ConversationDamage`].
//!
//! Pure decision logic over the store; the runtime maps a [`Verdict`] to its
//! wire error codes and never re-opens the log after the lock is released.
use std::path::Path;

use serde::Serialize;

pub mod assess;
pub mod project;
#[cfg(test)]
mod tests;

pub use assess::assess_locked;
pub use project::project;

/// Stand-in for a user turn whose text was empty (an image-only paste; the
/// image is not kept in history). Every provider converter sends this instead
/// of an empty text — Anthropic 400s on one, and it wedges the channel.
/// Murmur persists it and the runtime's watermark check compares against it,
/// so both sides share this one definition.
pub const BLANK_USER_TURN: &str = "(no text \u{2014} an image was attached)";

/// The text murmur persists for a user turn: a blank turn becomes
/// [`BLANK_USER_TURN`], anything else is kept verbatim.
pub fn normalize_user_text(text: &str) -> &str {
    if text.trim().is_empty() {
        BLANK_USER_TURN
    } else {
        text
    }
}

/// Why a conversation's log cannot be trusted. Closed set; serialized with a
/// kebab-case `reason` tag, the strings §7 lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum ConversationDamage {
    Manifest { detail: String },
    Unparseable { lines: Vec<usize> },
    Signature,
    Chain,
    Anchor,
    LogRegressed,
    Watermark { detail: String },
}

impl ConversationDamage {
    /// The `reason` string this damage serializes with.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Manifest { .. } => "manifest",
            Self::Unparseable { .. } => "unparseable",
            Self::Signature => "signature",
            Self::Chain => "chain",
            Self::Anchor => "anchor",
            Self::LogRegressed => "log-regressed",
            Self::Watermark { .. } => "watermark",
        }
    }

    /// Human detail beyond the reason, empty when the reason says it all.
    pub fn detail(&self) -> String {
        match self {
            Self::Manifest { detail } | Self::Watermark { detail } => detail.clone(),
            Self::Unparseable { lines } => format!("lines {lines:?}"),
            _ => String::new(),
        }
    }
}

impl std::fmt::Display for ConversationDamage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let detail = self.detail();
        if detail.is_empty() {
            f.write_str(self.reason())
        } else {
            write!(f, "{}: {detail}", self.reason())
        }
    }
}

/// Outcome of the locked read.
#[derive(Debug)]
pub enum Verdict {
    /// A `mur channel repair` began and has not finished.
    Repairing,
    Damaged(ConversationDamage),
    Ok(Assessment),
}

/// What a healthy locked read hands the runtime.
#[derive(Debug)]
pub struct Assessment {
    /// Human / `Agent{self}` messages (and S1 shell Notes) with
    /// `seq < channel_seq`, in seq order.
    pub projected: Vec<ProjectedMessage>,
    /// Last `seq` in the log (the log's own progress).
    pub log_last_seq: u64,
    /// `Some(old_watermark)`: a verified repair restarted the chain and the
    /// runtime cache must be discarded.
    pub rebuilt_after_repair: Option<u64>,
    /// `anchor_dirty` was set: proceed, audit-warn.
    pub anchor_dirty: bool,
}

/// One message of the projected history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedMessage {
    pub seq: u64,
    pub role: Role,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Agent,
}

/// Who is reading, and the key their channel writer signs with.
pub struct Keys<'a> {
    pub mur_home: &'a Path,
    /// This runtime's agent (canonical name).
    pub agent: &'a str,
    /// This agent's current channel writer key.
    pub writer_pubkey: [u8; 32],
}

/// The runtime cache's view of a conversation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheState {
    /// Highest seq the cache folded; `None` = cache miss.
    pub watermark: Option<u64>,
}
