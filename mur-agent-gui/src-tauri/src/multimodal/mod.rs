//! Multimodal drag-drop pipeline (D3, roadmap §4.3).
//!
//! Capture (drop / paste) → sandboxed decode → OCR → Unicode scrubber
//! → provenance ledger → B0SafetyHook turn-flag.

pub mod decode;
pub mod decoder_protocol;
pub mod dedupe;
pub mod heic;
pub mod unicode_scrubber;

pub use decode::DecoderClient;
pub use dedupe::DropDeduper;
