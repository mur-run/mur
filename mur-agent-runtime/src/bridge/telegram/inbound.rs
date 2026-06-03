//! Telegram inbound long-poll loop.
//!
//! [`TelegramInboundLoop`] runs `getUpdates(timeout = 50)` in a single
//! tokio task. It dedupes by `(bridge_id, update_id)` via the C1
//! [`crate::bridge::dedupe::DedupeStore`], gates on
//! [`mur_common::bridge::PrivacyMode`], signs the canonical-JSON
//! payload with the bridge identity, and advances the
//! [`crate::bridge::ack::AckTracker`] cursor only on a 2xx from the
//! user agent.
//!
//! The trait-based [`TgBotLike`] abstraction lets us mock the Bot API
//! for tests without spinning up real HTTPS infrastructure.
//! `RealBot = Throttle<CacheMe<Bot>>` is the production wiring; the
//! [`crate::bridge::telegram::mock::MockBot`] is the test double.

use std::path::PathBuf;

use teloxide::Bot;
use teloxide::adaptors::throttle::Limits;
use teloxide::adaptors::{CacheMe, Throttle};

use crate::bridge::ack::AckTracker;
use crate::bridge::dedupe::DedupeStore;
use crate::bridge::telegram::files::{FilesDeps, handle_document_update, handle_photo_update};
use crate::bridge::telegram::mock::{MockBot, MockUpdate, MockUserAgentHandle};
use crate::bridge::telegram::voice::{ForwardPayload, VoiceDeps, handle_voice_update};
use mur_common::bridge::{PrivacyMode, TelegramConfig};
use mur_common::identity::AgentIdentity;

/// Capability surface every Telegram-bot impl must satisfy. The real
/// production bot is `Throttle<CacheMe<Bot>>` (see [`build_real_bot`]);
/// tests use [`crate::bridge::telegram::mock::MockBot`].
///
/// We deliberately keep this minimal — only the methods the inbound
/// loop actually invokes. The detailed teloxide surface
/// (`get_updates` / `get_file` / `send_message`) is wired through in
/// later milestones (M-c2.3+).
pub trait TgBotLike: Send + Sync + 'static {}

/// Production bot type: `Throttle<CacheMe<Bot>>`. `Throttle` enforces
/// the Telegram API per-chat / global rate limits; `CacheMe` caches
/// the result of `getMe` so we avoid extra round-trips on startup.
pub type RealBot = Throttle<CacheMe<Bot>>;

/// Build a production bot wired with `Throttle` + `CacheMe`. The
/// `token` must be a BotFather-issued bot token (numeric:alphanum).
///
/// Uses [`Throttle::new_spawn`] which spawns the rate-limit worker on
/// the current tokio runtime — callers MUST be inside a tokio
/// `Runtime` (the C2 supervisor task is).
///
/// Note: this is a thin convenience wrapper; the C2 supervisor calls
/// it after resolving the token from the macOS keychain (see
/// [`mur_common::bridge::TelegramConfig::bot_token_keychain_account`]).
pub fn build_real_bot(token: &str) -> RealBot {
    Throttle::new_spawn(CacheMe::new(Bot::new(token)), Limits::default())
}

impl TgBotLike for RealBot {}

/// Long-poll inbound loop. Owns the bot handle, the long-poll cursor
/// (`offset`), and an optional [`InboundDeps`] bag (production code
/// always supplies it; the [`TelegramInboundLoop::stub_new`]
/// constructor used by the M-c2.2.2 skeleton test omits it).
pub struct TelegramInboundLoop<B: TgBotLike> {
    pub(crate) bot: B,
    pub(crate) offset: i64,
    pub(crate) deps: Option<InboundDeps>,
}

impl<B: TgBotLike> TelegramInboundLoop<B> {
    /// Skeleton constructor — used only by the M-c2.2.2 construction
    /// smoke test. Does NOT wire dedupe/ack/identity, so `tick_once`
    /// would panic if called against a stub-built loop.
    pub fn stub_new(bot: B) -> Self {
        Self {
            bot,
            offset: 0,
            deps: None,
        }
    }

    /// Current long-poll cursor. Advances iff the user agent
    /// confirmed the previous update with a 2xx response.
    pub fn offset(&self) -> i64 {
        self.offset
    }
}

/// Dependencies the production inbound loop needs to actually deliver
/// messages. Carried as `Option<InboundDeps>` on the loop so the
/// `stub_new` skeleton constructor stays cheap.
pub struct InboundDeps {
    pub config: TelegramConfig,
    pub dedupe: DedupeStore,
    pub ack: AckTracker<i64>,
    /// The bridge's signing identity. Used to sign every outbound
    /// envelope before forwarding to the user agent.
    pub identity: AgentIdentity,
    /// Key version stamped onto every signed envelope. Should match
    /// the one published in the bridge's Agent Card.
    pub key_version: u32,
    /// Flip to simulate "user agent always returns 5xx" for the
    /// offset-pinned-on-5xx test. Production code wires the real
    /// A2A client and ignores this flag.
    pub always_5xx: bool,
    /// Optional in-process user agent for routing tests (M-c2.2.4).
    /// Production code substitutes the real local A2A client.
    pub user_agent: Option<MockUserAgentHandle>,
    /// Where the voice handler stages downloaded audio. Defaults to a
    /// per-process tempdir in [`TelegramInboundLoop::default_test_deps`];
    /// production code points it at the agent's `~/.mur/agents/<name>`.
    pub agent_home: PathBuf,
    /// Test-only short-circuit forwarded to
    /// [`crate::bridge::telegram::voice::VoiceDeps::whisper_stub`].
    /// Production wires `None` and lets the voice handler invoke the
    /// real whisper backend.
    pub whisper_stub: Option<String>,
}

impl TelegramInboundLoop<MockBot> {
    /// Construct a fully-wired test loop. The production
    /// supervisor will use a parallel constructor that takes
    /// [`RealBot`] and a real A2A client handle; that lands in
    /// later milestones (M-c2.6+).
    pub fn new(bot: MockBot, deps: InboundDeps) -> Self {
        Self {
            bot,
            offset: 0,
            deps: Some(deps),
        }
    }

    /// Default test [`InboundDeps`] — DM-only, in-memory dedupe, fresh
    /// identity, no user agent, voice staged into a per-process
    /// tempdir. Tests typically mutate one or two fields after
    /// construction.
    pub fn default_test_deps() -> InboundDeps {
        InboundDeps {
            config: TelegramConfig {
                bot_username: "B".into(),
                bot_token_keychain_account: "x".into(),
                chat_id: 100,
                privacy_mode: PrivacyMode::DmOnly,
                allow_groups: vec![],
                e2e_disclosure_acked_at: None,
            },
            dedupe: DedupeStore::in_memory().expect("in-memory dedupe"),
            ack: AckTracker::<i64>::new(0),
            identity: AgentIdentity::generate(),
            key_version: 0,
            always_5xx: false,
            user_agent: None,
            // Per-process scratch dir; the OS reaps it when the
            // process exits. Tests that care about the on-disk audit
            // trail can override this.
            agent_home: std::env::temp_dir().join(format!("mur-c2-test-{}", std::process::id())),
            whisper_stub: None,
        }
    }

    /// Drain queued mock updates and run them through the full
    /// inbound pipeline: dedupe → privacy gate → voice transcribe (if
    /// applicable) → sign → forward → ACK. Returns the number of
    /// updates that were successfully delivered.
    pub async fn tick_once(&mut self) -> anyhow::Result<usize> {
        let updates: Vec<MockUpdate> =
            std::mem::take(&mut *self.bot.queued_updates.lock().unwrap());
        let deps = self
            .deps
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("tick_once called on stub-built loop"))?;
        let mut delivered = 0usize;

        for u in updates {
            // (1) update_id monotonicity — Telegram's getUpdates(offset)
            // contract; if a stale poll surfaces an already-processed
            // id, drop it on the floor.
            if u.id <= self.offset {
                continue;
            }

            // (2) dedupe by (bridge_id, update_id). The DedupeStore
            // namespace is keyed on bridge_id internally; we use a
            // `tg/<update_id>` token as the message id.
            let dedupe_key = format!("tg/{}", u.id);
            if deps.dedupe.is_seen(&dedupe_key)? {
                continue;
            }

            // (3) privacy gate. A Telegram bot receives DMs from *anyone* who
            // starts it, so `is_private` alone is not authorization — the DM
            // must come from the configured owner `chat_id`. `AllowGroups`
            // additionally accepts groups explicitly listed in `allow_groups`.
            let allowed = match deps.config.privacy_mode {
                PrivacyMode::DmOnly => u.is_private && u.chat_id == deps.config.chat_id,
                PrivacyMode::AllowGroups => {
                    (u.is_private && u.chat_id == deps.config.chat_id)
                        || deps.config.allow_groups.contains(&u.chat_id)
                }
            };
            if !allowed {
                continue;
            }

            // (4) Inbound payload resolution. Order matters: voice
            // updates arrive with `voice_file_id` only; documents have
            // `document_file_id`; photos have `photo_file_id`. Each
            // path may stage an on-disk artifact and surface a sha256
            // for the outbound envelope. Errors here abort delivery
            // (cursor stays pinned, dedupe not yet marked) so the
            // bridge can retry on the next poll.
            let mut artifact_sha256: Option<String> = None;
            let body: String = if u.voice_file_id.is_some() {
                let voice_deps = VoiceDeps {
                    agent_home: deps.agent_home.clone(),
                    whisper_stub: deps.whisper_stub.clone(),
                };
                match handle_voice_update(&self.bot, &u, &voice_deps).await {
                    Ok(ForwardPayload::Text { transcript, .. }) => transcript,
                    Ok(ForwardPayload::Skip) => continue,
                    Err(_) => {
                        // Drop this update without marking-seen so the
                        // next poll can retry. We deliberately do NOT
                        // advance the offset.
                        continue;
                    }
                }
            } else if u.document_file_id.is_some() {
                let fdeps = FilesDeps {
                    agent_home: deps.agent_home.clone(),
                    // Real wiring derives this from
                    // `update.document.mime_type`; the synthetic
                    // MockUpdate doesn't carry it, so we conservatively
                    // tag every document `application/pdf` (the
                    // dominant case in the M-c2.4 acceptance).
                    mime: "application/pdf".into(),
                };
                match handle_document_update(&self.bot, &u, &fdeps).await {
                    Ok(res) => {
                        artifact_sha256 = Some(res.sha256);
                        u.caption.clone().unwrap_or_default()
                    }
                    Err(_) => continue,
                }
            } else if u.photo_file_id.is_some() {
                let fdeps = FilesDeps {
                    agent_home: deps.agent_home.clone(),
                    // Telegram normalises uploaded photos to JPEG
                    // regardless of source format; PNG is sent as a
                    // document. Pin the mime here so OCR plumbing
                    // downstream (D-track) doesn't have to guess.
                    mime: "image/jpeg".into(),
                };
                match handle_photo_update(&self.bot, &u, &fdeps).await {
                    Ok(res) => {
                        artifact_sha256 = Some(res.sha256);
                        u.caption.clone().unwrap_or_default()
                    }
                    Err(_) => continue,
                }
            } else {
                u.text.clone().unwrap_or_default()
            };

            // (5) mark seen BEFORE we try to deliver — we'd rather
            // double-drop a duplicate than double-deliver a real
            // message on retry.
            deps.dedupe.mark_seen(&dedupe_key)?;

            // (6) hand off to the user agent + ACK. If the configured
            // mock-user-agent returned a 2xx we advance the cursor;
            // 5xx leaves it pinned so the next poll re-fetches.
            deps.ack.start_pending(u.id);

            let outcome = deliver_to_user_agent(deps, &u, &body, artifact_sha256.as_deref()).await;
            match outcome {
                Ok(()) => {
                    deps.ack.confirm();
                    self.offset = u.id;
                    delivered += 1;
                }
                Err(_) => {
                    deps.ack.reject();
                    // Cursor stays pinned; the next poll re-surfaces
                    // this update (and the dedupe store will short-
                    // circuit it once the user agent recovers).
                }
            }
        }

        Ok(delivered)
    }
}

/// Build, sign, and forward an envelope to the configured user agent.
/// Returns `Err` for any non-2xx response; the inbound loop interprets
/// `Err` as "do NOT advance the cursor". The `body` is computed by the
/// caller — text updates use `update.text`; voice updates substitute
/// the whisper transcript (M-c2.3); document/photo updates substitute
/// the caption (M-c2.4) and pass the sha256 of the staged artifact via
/// `artifact_sha256`.
async fn deliver_to_user_agent(
    deps: &InboundDeps,
    u: &MockUpdate,
    body: &str,
    artifact_sha256: Option<&str>,
) -> anyhow::Result<()> {
    if deps.always_5xx {
        anyhow::bail!("user-agent returned 5xx (simulated)");
    }

    // Construct the JSON-RPC `message/send` request the user agent
    // expects. Field order is fixed by `canonicalize_json` (sorted
    // keys); the byte sequence we sign is the byte sequence the
    // verifier reproduces. We always emit `artifact_sha256` (empty
    // string for non-multimodal turns) so the canonical schema is
    // stable across update types — the user-agent side keys off
    // `is_empty()` to decide whether to look up a ledger entry.
    let payload = serde_json::json!({
        "id": u.id,
        "jsonrpc": "2.0",
        "method": "message/send",
        "params": {
            "agent": deps.config.bot_username,
            "artifact_sha256": artifact_sha256.unwrap_or(""),
            "body": body,
        },
    });
    let canonical = canonicalize_json(&payload);
    let envelope =
        mur_common::bridge::envelope::sign_payload(canonical, &deps.identity, deps.key_version);

    if let Some(ua) = &deps.user_agent {
        ua.send(envelope, "message/send").await?;
    }
    Ok(())
}

/// Local canonical-JSON encoder: sorted keys, no whitespace. Matches
/// the algorithm `mur_common::identity` uses internally so signers
/// and verifiers compute identical byte sequences.
fn canonicalize_json(value: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_canonical(&mut out, value);
    out
}

fn write_canonical(out: &mut Vec<u8>, v: &serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => {
            let escaped = serde_json::to_string(s).unwrap();
            out.extend_from_slice(escaped.as_bytes());
        }
        Value::Array(arr) => {
            out.push(b'[');
            for (i, e) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(out, e);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                let key_escaped = serde_json::to_string(k).unwrap();
                out.extend_from_slice(key_escaped.as_bytes());
                out.push(b':');
                write_canonical(out, &map[*k]);
            }
            out.push(b'}');
        }
    }
}
