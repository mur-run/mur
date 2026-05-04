//! Telegram voice-message handler (M-c2.3, future work).
//!
//! Currently a stub — the inbound loop will surface `voice_file_id`
//! values via [`crate::bridge::telegram::mock::MockUpdate`] for tests,
//! but no transcription pipeline is wired yet.
