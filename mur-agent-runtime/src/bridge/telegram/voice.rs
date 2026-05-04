//! Telegram voice-message handler (M-c2.3).
//!
//! Pipeline: `getFile` download (currently mocked via
//! [`MockBot::fetch_file`]) → save to
//! `<agent_home>/telemetry/inputs/voice/{file_id}.ogg` → transcribe
//! (whisper-rs in production; stubbed in tests via
//! [`VoiceDeps::whisper_stub`]) → return `{transcript, audio_path}`
//! for the inbound loop to forward to the user agent as the
//! `message/send` body.
//!
//! The 30 MB cap mirrors Telegram's voice-message ceiling (their
//! published limit is 20 MB; we round up to 30 MB to leave headroom
//! for header overhead and forward-compat with future Bot API limit
//! lifts).
//!
//! The real whisper-rs backend is intentionally NOT wired here yet —
//! the workspace doesn't carry a whisper-rs dep, and the production
//! supervisor will gate transcription behind a deployment-time choice
//! (CLI helper / external service / local model). When that lands, the
//! `whisper_stub.is_none()` branch below should be replaced with the
//! real call.

use std::path::PathBuf;

use crate::bridge::telegram::mock::{MockBot, MockUpdate};

/// Telegram's published voice-message ceiling is 20 MB. We round up to
/// 30 MB to leave headroom for header overhead and to future-proof
/// against a Bot API limit lift.
pub const VOICE_MAX_BYTES: u64 = 30_000_000;

/// Transcription backend + on-disk staging for [`handle_voice_update`].
///
/// `whisper_stub` lets tests bypass the real (heavy) whisper-rs
/// pipeline. Production code passes `None` here, which today causes
/// [`handle_voice_update`] to bail with a clear "not yet wired"
/// error — that wiring lands in a follow-up milestone once the
/// workspace whisper-rs choice is made.
pub struct VoiceDeps {
    /// Agent home directory. The handler writes downloaded audio to
    /// `<agent_home>/telemetry/inputs/voice/<file_id>.ogg` so that the
    /// audit trail captures the raw bytes before transcription.
    pub agent_home: PathBuf,
    /// Test-only short-circuit. When `Some(s)`, the handler skips
    /// transcription entirely and returns `s` as the transcript.
    pub whisper_stub: Option<String>,
}

/// What [`handle_voice_update`] returns to the inbound loop.
///
/// `Text` is the success case — the caller substitutes `transcript`
/// for the `params.body` field of the outbound `message/send`
/// envelope. `Skip` is reserved for future heuristics (e.g. silent
/// audio, transcription confidence below threshold) that should drop
/// the update without an error.
#[derive(Debug)]
pub enum ForwardPayload {
    Text {
        transcript: String,
        audio_path: PathBuf,
    },
    Skip,
}

/// Download a Telegram voice message, persist it under
/// `<agent_home>/telemetry/inputs/voice/`, and return the transcript.
///
/// Errors:
///
/// - `voice_file_id` missing on the update (caller bug)
/// - file size exceeds [`VOICE_MAX_BYTES`]
/// - `MockBot::fetch_file` returns no bytes (`getFile` 404 in prod)
/// - filesystem write failure under `agent_home`
/// - real whisper backend not yet wired and `whisper_stub` is `None`
pub async fn handle_voice_update(
    bot: &MockBot,
    update: &MockUpdate,
    deps: &VoiceDeps,
) -> anyhow::Result<ForwardPayload> {
    // (1) size cap — we check the Telegram-reported `file_size` BEFORE
    // hitting the network. The real `getFile` response carries this
    // field; the MockUpdate mirrors it.
    if let Some(sz) = update.file_size
        && sz > VOICE_MAX_BYTES
    {
        anyhow::bail!(
            "voice file too large: {} bytes (cap {})",
            sz,
            VOICE_MAX_BYTES
        );
    }

    let file_id = update
        .voice_file_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no voice file_id on update"))?;

    // (2) download. In production this is `bot.get_file(file_id)` →
    // download URL → GET; the mock collapses both round-trips.
    let bytes = bot.fetch_file(file_id)?;

    // (3) persist to <agent_home>/telemetry/inputs/voice/<file_id>.ogg.
    // We deliberately keep the raw bytes around so that an operator
    // re-running transcription against an updated whisper model can
    // replay the original audio without re-fetching from Telegram
    // (whose file URLs expire ~1h after `getFile`).
    let dir = deps.agent_home.join("telemetry/inputs/voice");
    std::fs::create_dir_all(&dir)?;
    let audio_path = dir.join(format!("{file_id}.ogg"));
    std::fs::write(&audio_path, &bytes)?;

    // (4) transcribe. Tests stub this; production wires whisper-rs in
    // a follow-up milestone (see module-level docs).
    let transcript = if let Some(s) = &deps.whisper_stub {
        s.clone()
    } else {
        anyhow::bail!(
            "real whisper transcription not yet wired; set VoiceDeps::whisper_stub in tests"
        );
    };

    Ok(ForwardPayload::Text {
        transcript,
        audio_path,
    })
}
