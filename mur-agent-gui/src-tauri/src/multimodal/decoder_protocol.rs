//! JSON-RPC protocol between the GUI process and the
//! `mur-agent-decoder` subprocess.
//!
//! Wire format: length-prefixed frames. Each frame is a 4-byte
//! big-endian length followed by that many bytes of UTF-8 JSON.
//! Same shape as M0a5's TCP Noise framing, different bytes.

use serde::{Deserialize, Serialize};

/// Request sent from the GUI process to the sandboxed decoder
/// subprocess. The subprocess reads exactly one request frame, decodes,
/// writes one response frame, exits.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum DecodeRequest {
    Image { bytes: Vec<u8>, mime_hint: String },
    Pdf { bytes: Vec<u8> },
}

/// Response from the decoder subprocess back to the GUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum DecodeResponse {
    /// Image path: PNG sRGB 8-bit re-encoded from the raw RGBA buffer
    /// (EXIF / XMP / iCCP / thumbnails dropped by re-encoding).
    Ok {
        png_bytes: Vec<u8>,
        decoder_version: String,
    },
    /// PDF path: per-page extracted text + < 1pt quarantine flag.
    PdfText {
        pages: Vec<PdfPageText>,
        decoder_version: String,
    },
    Error(DecodeError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PdfPageText {
    pub page: u32,
    pub text: String,
    /// True when any glyph on this page was rendered at < 1pt — likely
    /// an attempt at invisible-text injection. Caller treats as
    /// quarantined (still wrapped, but tagged separately).
    pub quarantined: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DecodeError {
    UnsupportedFormat {
        mime: String,
    },
    DecodeFailed {
        reason: String,
    },
    /// Hit the per-file size cap (configurable, default 30 MB).
    SizeLimitExceeded {
        limit_bytes: u64,
    },
    /// Decoder timed out (default 10s).
    Timeout,
}

/// Length-prefixed framed write helper.
pub fn write_frame<W: std::io::Write>(w: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    let len = (bytes.len() as u32).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(bytes)?;
    Ok(())
}

/// Length-prefixed framed read helper.
///
/// Caps at 64 MiB to defend against a malicious decoder claiming a huge
/// frame.
pub fn read_frame<R: std::io::Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > 64 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}
