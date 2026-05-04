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
use std::time::{Duration, Instant};

use mur_common::bridge::envelope::{SignedEnvelope, verify_envelope_with_pubkey};

use super::inbound::TgBotLike;

/// Throttle policy applied by [`MockBot::send_message`] before recording
/// the `(chat_id, body)` pair on `sent_messages`.
///
/// Two modes mirror the two rate-limits enforced by teloxide's
/// `Throttle` adaptor:
///
/// - [`ThrottlePolicy::Global`] caps the *aggregate* send-rate across
///   all chats (the Bot API's 30 messages/second/bot ceiling).
/// - [`ThrottlePolicy::PerChat`] paces per-chat at 1 message/second
///   (the Bot API's per-chat ceiling for groups).
///
/// `None` (the default) is a fast-path that simply records the send;
/// preserved so existing tests using `MockBot::default()` see no extra
/// latency.
#[derive(Debug, Clone, Copy)]
pub enum ThrottlePolicy {
    /// Token-bucket: at most `rate_per_sec` calls within any rolling
    /// 1 s window. Implemented as a simple "wait for the
    /// `rate_per_sec`-th-most-recent send to age past 1 s" algorithm —
    /// good enough for the ≤30/s cap test.
    Global { rate_per_sec: u32 },
    /// Per-chat pacing: each chat enforces at most `rate_per_sec`
    /// messages per rolling second. Implemented by tracking
    /// `(chat_id → last_send Instant)` and sleeping the difference.
    PerChat { rate_per_sec: u32 },
}

/// Test stand-in for `Throttle<CacheMe<teloxide::Bot>>`. Owns a queue
/// of synthetic updates and a log of `send_message` calls. All inner
/// state is `Mutex`-protected so test code can poke it from the same
/// thread as `tick_once`.
///
/// ## Throttle modes (M-c2.6)
///
/// `MockBot::default()` is unthrottled — sends are appended
/// synchronously to `sent_messages`. `MockBot::throttled(rate)` and
/// `MockBot::per_chat(rate)` install a [`ThrottlePolicy`]; the
/// outbound MCP dispatcher in [`super::mcp::handle_jsonrpc`] honours
/// the policy by awaiting [`MockBot::send_message`] (which sleeps
/// when the policy demands).
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

    /// Optional rate-limit policy; `None` means no throttling.
    /// See [`ThrottlePolicy`] for semantics.
    pub policy: Option<ThrottlePolicy>,
    /// Per-chat last-send wall-clock instants — driven by
    /// [`ThrottlePolicy::PerChat`].
    per_chat_last: Mutex<HashMap<i64, Instant>>,
    /// Rolling timestamp log used by [`ThrottlePolicy::Global`] to
    /// enforce the aggregate cap. Each entry is the wall-clock instant
    /// at which `send_message` recorded the corresponding push to
    /// `sent_messages`.
    global_send_log: Mutex<Vec<Instant>>,
    /// Wall-clock of the very first `send_message` call. `None` until
    /// the first send, so [`MockBot::delivered_within`] can return 0
    /// before any traffic.
    first_send_at: Mutex<Option<Instant>>,
}

impl MockBot {
    /// Construct a [`MockBot`] with a global token-bucket cap of
    /// `rate_per_sec` outbound `send_message` calls (M-c2.6.1).
    pub fn throttled(rate_per_sec: u32) -> Self {
        Self {
            policy: Some(ThrottlePolicy::Global { rate_per_sec }),
            ..Self::default()
        }
    }

    /// Construct a [`MockBot`] with a per-chat pacing cap of
    /// `rate_per_sec` (M-c2.6.2). Each `chat_id` independently enforces
    /// the cap; concurrent chats do not block each other.
    pub fn per_chat(rate_per_sec: u32) -> Self {
        Self {
            policy: Some(ThrottlePolicy::PerChat { rate_per_sec }),
            ..Self::default()
        }
    }

    /// Throttle-aware send. Honours [`MockBot::policy`] before pushing
    /// onto `sent_messages`. The MCP dispatcher calls this from
    /// `tools/call`; old tests that drive `sent_messages` directly are
    /// unaffected.
    pub async fn send_message(&self, chat_id: i64, body: String) {
        if let Some(p) = self.policy {
            self.apply_policy(p, chat_id).await;
        }
        let now = Instant::now();
        {
            let mut first = self.first_send_at.lock().unwrap();
            if first.is_none() {
                *first = Some(now);
            }
        }
        self.global_send_log.lock().unwrap().push(now);
        self.sent_messages.lock().unwrap().push((chat_id, body));
    }

    async fn apply_policy(&self, policy: ThrottlePolicy, chat_id: i64) {
        match policy {
            ThrottlePolicy::Global { rate_per_sec } => {
                // Token bucket: the Nth send within a 1 s window
                // (where N == rate_per_sec) must wait for the oldest
                // entry in the current window to age past 1 s.
                let window = Duration::from_secs(1);
                let rate = rate_per_sec as usize;
                loop {
                    let sleep_for = {
                        let mut log = self.global_send_log.lock().unwrap();
                        let now = Instant::now();
                        // Drop entries older than the window.
                        log.retain(|t| now.duration_since(*t) < window);
                        if log.len() < rate {
                            None
                        } else {
                            // Wait for the oldest in-window entry to expire.
                            let oldest = *log.first().expect("len >= rate >= 1");
                            Some(window.saturating_sub(now.duration_since(oldest)))
                        }
                    };
                    match sleep_for {
                        None => break,
                        // Add a 1 ms safety margin so the next loop
                        // sees the entry as expired (avoids busy-loop
                        // due to Instant granularity).
                        Some(d) => tokio::time::sleep(d + Duration::from_millis(1)).await,
                    }
                }
            }
            ThrottlePolicy::PerChat { rate_per_sec } => {
                // 1 / rate_per_sec gap per chat.
                let gap = Duration::from_secs_f64(1.0 / rate_per_sec.max(1) as f64);
                let sleep_for = {
                    let map = self.per_chat_last.lock().unwrap();
                    map.get(&chat_id).map(|last| {
                        let elapsed = Instant::now().duration_since(*last);
                        gap.saturating_sub(elapsed)
                    })
                };
                if let Some(d) = sleep_for
                    && !d.is_zero()
                {
                    tokio::time::sleep(d).await;
                }
                self.per_chat_last
                    .lock()
                    .unwrap()
                    .insert(chat_id, Instant::now());
            }
        }
    }

    /// Count how many `send_message` calls have completed within
    /// `window` of the very first call. Used by M-c2.6.1 to assert the
    /// global cap is honoured. Returns 0 if no sends have happened
    /// yet.
    pub fn delivered_within(&self, window: Duration) -> usize {
        let first = match *self.first_send_at.lock().unwrap() {
            Some(t) => t,
            None => return 0,
        };
        let cutoff = first + window;
        self.global_send_log
            .lock()
            .unwrap()
            .iter()
            .filter(|t| **t <= cutoff)
            .count()
    }

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
