//! The answerer's checks on an `Ask`, in design order (§D3.1–D3.4). Pure over
//! the channel log and the clock, so each refusal is testable on its own.
//!
//! Every failure is a [`Refusal`] that names its reason; nothing falls back to
//! a default. The dialer is never consulted: the `Ask` authenticates itself.

use std::path::Path;

use mur_common::channel::{AskPayload, ChannelActor, ChannelEvent, EventKind};

/// Why a consult was refused. The text is what the dialer sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// §D3.0: the answerer's runner is not the built-in loop.
    Backend(&'static str),
    /// R6: this platform does not kernel-protect an agent's own profile.
    Platform(&'static str),
    /// The `channel_id` param is not a safe channel id.
    BadChannel(String),
    /// §D3.1: no `Ask` carries the nonce.
    NotFound,
    /// §D3.1: more than one `Ask` carries the nonce.
    Ambiguous(usize),
    /// §D3.2: the actor is not an agent, or its id is not a valid agent name.
    BadActor(String),
    /// §D3.3: the signature does not verify against the actor's key.
    Signature(String),
    /// The `Ask` payload does not parse, or its nonce disagrees with the key.
    Malformed(String),
    /// §D3.4: addressed to another agent.
    NotAddressed { to: String },
    /// §D3.4: past `expires_at_ms`.
    Expired { expires_at_ms: i64 },
    /// §D3.5: this `(asker, nonce)` was already answered.
    Replayed { asker: String },
    /// §D3.6: the asker's profile cannot be read or parsed.
    Scope { asker: String, detail: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(b) => write!(
                f,
                "consult refused: this agent runs on the `{b}` backend; only the built-in loop can confine a consult turn"
            ),
            Self::Platform(why) => write!(f, "consult refused: {why}"),
            Self::BadChannel(id) => write!(f, "consult refused: invalid channel id {id:?}"),
            Self::NotFound => write!(f, "consult refused: no Ask with this nonce in the channel"),
            Self::Ambiguous(n) => write!(
                f,
                "consult refused: {n} Ask events share this nonce; refusing to pick one"
            ),
            Self::BadActor(a) => write!(
                f,
                "consult refused: the Ask's actor {a} is not a valid agent"
            ),
            Self::Signature(a) => write!(
                f,
                "consult refused: the Ask's signature does not verify against {a}'s key"
            ),
            Self::Malformed(why) => write!(f, "consult refused: malformed Ask: {why}"),
            Self::NotAddressed { to } => {
                write!(
                    f,
                    "consult refused: the Ask is addressed to {to:?}, not this agent"
                )
            }
            Self::Expired { expires_at_ms } => write!(
                f,
                "consult refused: the Ask expired at {expires_at_ms} (Unix ms)"
            ),
            Self::Replayed { asker } => write!(
                f,
                "consult refused: this Ask from {asker} was already answered"
            ),
            Self::Scope { asker, detail } => write!(
                f,
                "consult refused: cannot derive {asker}'s read scope from its profile: {detail}"
            ),
        }
    }
}

/// An `Ask` that passed §D3.1–D3.4.
#[derive(Debug, Clone)]
pub struct VerifiedAsk {
    pub asker: String,
    pub payload: AskPayload,
}

/// R6: refuse where an agent's own `profile.yaml` is not write-protected by
/// the kernel. macOS enforces it (`sandbox/macos.rs`); Linux waits on P2 and
/// Windows has not been examined.
pub fn platform_refusal() -> Option<Refusal> {
    if cfg!(target_os = "macos") {
        None
    } else if cfg!(target_os = "linux") {
        Some(Refusal::Platform(
            "on Linux an agent can still rewrite its own profile.yaml through bash (the Landlock cage has no deny-within-allow), so the asker's read scope cannot be trusted there yet",
        ))
    } else {
        Some(Refusal::Platform(
            "this platform has not been checked for kernel protection of an agent's own profile.yaml",
        ))
    }
}

/// §D3.1–D3.4 over `events`, the channel's parsed log.
pub fn verify_ask(
    mur_home: &Path,
    channel_id: &str,
    events: &[ChannelEvent],
    nonce: &str,
    answerer: &str,
    now_ms: i64,
) -> Result<VerifiedAsk, Refusal> {
    // 1. Locate exactly one. The store dedupes on the key, but a raw append
    //    does not go through the store.
    let mut found = events
        .iter()
        .filter(|e| e.kind == EventKind::Ask && e.idempotency_key.as_deref() == Some(nonce));
    let ev = found.next().ok_or(Refusal::NotFound)?;
    let extra = found.count();
    if extra > 0 {
        return Err(Refusal::Ambiguous(extra + 1));
    }

    // 2. Actor: an agent with a valid name, before any path join.
    let asker = match &ev.actor {
        ChannelActor::Agent { id } => id.clone(),
        other => return Err(Refusal::BadActor(format!("{other:?}"))),
    };
    if mur_common::validate_agent_name(&asker).is_err() {
        return Err(Refusal::BadActor(format!("{asker:?}")));
    }

    // 3. Signature, required whatever MUR_CHANNEL_REQUIRE_SIG says.
    if !mur_channel::verify::verify_event(mur_home, channel_id, ev, true) {
        return Err(Refusal::Signature(asker));
    }

    // Signed, so the payload is the asker's own words from here on.
    let payload: AskPayload = serde_json::from_value(ev.payload.clone())
        .map_err(|e| Refusal::Malformed(e.to_string()))?;
    if payload.nonce != nonce {
        return Err(Refusal::Malformed(
            "the payload nonce differs from the idempotency key".into(),
        ));
    }

    // 4. Binding.
    if payload.to != answerer {
        return Err(Refusal::NotAddressed { to: payload.to });
    }
    if now_ms > payload.expires_at_ms {
        return Err(Refusal::Expired {
            expires_at_ms: payload.expires_at_ms,
        });
    }

    Ok(VerifiedAsk { asker, payload })
}

/// Wall-clock Unix milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
