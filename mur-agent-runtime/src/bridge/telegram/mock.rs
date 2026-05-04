//! Test doubles for the Telegram inbound loop.
//!
//! - [`MockBot`] satisfies [`super::inbound::TgBotLike`] without any
//!   network. Tests push synthetic [`MockUpdate`] entries onto a
//!   queue; `tick_once` drains them in FIFO order.
//! - [`MockUpdate`] mirrors just enough of `teloxide::types::Update`
//!   for routing semantics — id, chat, kind discriminators.
//! - [`MockUserAgent`] / [`MockUserAgentHandle`] capture forwarded
//!   envelopes so M-c2.2.4 can assert "the user agent received a
//!   verifiable signed payload".

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mur_common::bridge::envelope::{SignedEnvelope, verify_envelope_with_pubkey};

use super::inbound::TgBotLike;

/// Test stand-in for `Throttle<CacheMe<teloxide::Bot>>`. Owns a queue
/// of synthetic updates and a log of `send_message` calls. All inner
/// state is `Mutex`-protected so test code can poke it from the same
/// thread as `tick_once`.
#[derive(Default)]
pub struct MockBot {
    /// Queued synthetic updates. The inbound loop drains this on each
    /// call to `tick_once`.
    pub queued_updates: Mutex<Vec<MockUpdate>>,
    /// Log of `(chat_id, body)` pairs the bridge would have sent
    /// outbound. Currently unused by the inbound loop, but exposed so
    /// future outbound milestones can assert side effects.
    pub sent_messages: Mutex<Vec<(i64, String)>>,
    /// Stubbed file-bytes returned by [`MockBot::fetch_file`]. Tests
    /// register `(file_id, bytes)` pairs ahead of time; the voice and
    /// document handlers (M-c2.3 / M-c2.4) look them up by `file_id`.
    /// We use a `HashMap` so the same bot can serve multiple files.
    pub file_blobs: Mutex<HashMap<String, Vec<u8>>>,
}

impl MockBot {
    /// Register a file payload that subsequent [`MockBot::fetch_file`]
    /// calls will return verbatim. Mirrors a successful
    /// `getFile` + binary download against the real Bot API.
    pub fn stub_file_bytes(&self, file_id: String, bytes: Vec<u8>) {
        self.file_blobs.lock().unwrap().insert(file_id, bytes);
    }

    /// Synchronous file fetch. Returns the bytes registered via
    /// [`MockBot::stub_file_bytes`], or an error if no stub matches —
    /// matching the production semantics of `getFile` + GET on the
    /// returned URL.
    pub fn fetch_file(&self, file_id: &str) -> anyhow::Result<Vec<u8>> {
        self.file_blobs
            .lock()
            .unwrap()
            .get(file_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no stubbed file for id={file_id}"))
    }
}

impl TgBotLike for MockBot {}

/// Synthetic Telegram update — covers the union of fields the C2 .2/.3/.4
/// milestones need (text, voice, document, photo, caption, file_size).
/// Each test pushes whatever subset it cares about; the rest stay
/// `None`.
#[derive(Clone, Debug)]
pub struct MockUpdate {
    pub id: i64,
    pub chat_id: i64,
    pub is_private: bool,
    pub text: Option<String>,
    pub voice_file_id: Option<String>,
    pub document_file_id: Option<String>,
    pub photo_file_id: Option<String>,
    pub caption: Option<String>,
    pub file_size: Option<u64>,
}

/// One forwarded envelope plus the JSON-RPC method we extracted from
/// it BEFORE signing. `verified` is the result of running
/// [`verify_envelope_with_pubkey`] against the bridge's public key
/// at the time of receipt — `true` proves both the signature and the
/// canonical-payload bytes survived the round-trip intact.
#[derive(Clone, Debug)]
pub struct ReceivedReq {
    pub method: String,
    pub envelope: SignedEnvelope,
    pub verified: bool,
}

/// In-process user-agent test double. Captures every signed envelope
/// the inbound loop forwards. Cloneable handles share the same
/// underlying `Vec<ReceivedReq>` so test code can hold the
/// "inspector" handle while the inbound loop holds a "writer" handle.
#[derive(Default)]
pub struct MockUserAgent {
    inner: Arc<Mutex<Vec<ReceivedReq>>>,
}

impl MockUserAgent {
    /// Take a writer-handle suitable for stuffing into
    /// [`super::inbound::InboundDeps::user_agent`]. The original
    /// `MockUserAgent` retains a reader handle for assertions.
    pub fn handle(&self) -> MockUserAgentHandle {
        MockUserAgentHandle {
            inner: self.inner.clone(),
        }
    }

    /// Snapshot every envelope received so far. Cheap clone — the
    /// `ReceivedReq` only owns the envelope bytes.
    pub fn received(&self) -> Vec<ReceivedReq> {
        self.inner.lock().unwrap().clone()
    }
}

/// Cloneable, `Send + Sync` writer handle for [`MockUserAgent`]. The
/// inbound loop uses this to record forwarded envelopes; production
/// code substitutes the real local-A2A client (which does not share
/// this trait surface — the substitution is structural via
/// `InboundDeps::user_agent: Option<MockUserAgentHandle>`).
#[derive(Clone)]
pub struct MockUserAgentHandle {
    inner: Arc<Mutex<Vec<ReceivedReq>>>,
}

impl MockUserAgentHandle {
    /// Verify the envelope against its embedded pubkey, then append
    /// it to the shared log. Returns `Ok(())` for any envelope that
    /// passes signature verification — we treat verification as the
    /// 2xx-equivalent for routing tests.
    pub async fn send(&self, envelope: SignedEnvelope, method: &str) -> anyhow::Result<()> {
        let verified =
            verify_envelope_with_pubkey(&envelope, &envelope.bridge_pubkey_multibase).is_ok();
        self.inner.lock().unwrap().push(ReceivedReq {
            method: method.to_string(),
            envelope,
            verified,
        });
        Ok(())
    }
}
