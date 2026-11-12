//! Where signing marks go (spec "Signing mark", writer).
//!
//! The proxy only decides *when* a mark is due: a signature came back from
//! the upstream under an `Allow` that carried a live decision. Signing and
//! appending it is the supervisor's callback, so this module does not depend
//! on the signing identity, the same split as [`super::refusal::RefusalSink`].

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use mur_ssh_request::SigningMark;

use super::gate::{LiveDecision, SignAsk};

/// Appends one mark to the agent's channel. Blocking; best-effort.
pub type MarkPost = Arc<dyn Fn(&SigningMark) + Send + Sync>;

/// Writes at most one mark per `(action_hash, issued_at)` per process.
#[derive(Default)]
pub struct MarkSink {
    post: Option<MarkPost>,
    seen: Mutex<HashSet<(String, DateTime<Utc>)>>,
}

impl MarkSink {
    pub fn new(post: MarkPost) -> Self {
        Self {
            post: Some(post),
            seen: Mutex::default(),
        }
    }

    /// A signature for `ask` left the proxy under `decision`. The write is
    /// spawned off the reply path, so it never delays the signature, and its
    /// failure never fails one.
    pub fn signed(&self, ask: &SignAsk, decision: &LiveDecision) {
        let Some(post) = &self.post else { return };
        if !decision.allow {
            return;
        }
        let first = self
            .seen
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert((decision.action_hash.clone(), decision.issued_at));
        if !first {
            return;
        }
        let mark = SigningMark::new(
            decision.action_hash.clone(),
            decision.issued_at,
            ask.key_fp.clone(),
            ask.host_key_fp.clone(),
        );
        let post = post.clone();
        tokio::task::spawn_blocking(move || post(&mark));
    }
}
