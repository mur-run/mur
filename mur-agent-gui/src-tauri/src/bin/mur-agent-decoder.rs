//! `mur-agent-decoder` — sandboxed image/PDF decoder subprocess.
//!
//! Reads exactly one `DecodeRequest` frame from stdin, runs the
//! appropriate decode, writes one `DecodeResponse` frame to stdout,
//! exits. Process isolation is the only mitigation in M3.2.2: a
//! malicious image that exploits libpng / libheif crashes only this
//! process, not the GUI. Real OS-level sandbox (macOS SBPL + Landlock)
//! lands in B1 (v2). PDF decode is M3.4.

use std::io;

use mur_agent_gui_lib::multimodal::decoder_protocol::{
    DecodeError, DecodeRequest, DecodeResponse, read_frame, write_frame,
};

const DECODER_VERSION: &str = concat!("image-rs/0.25 (host=", env!("CARGO_PKG_VERSION"), ")",);

fn main() {
    // TODO(B1): apply macOS sandbox profile + Landlock here. v2 milestone.

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();

    let request_bytes = match read_frame(&mut stdin) {
        Ok(b) => b,
        Err(_) => std::process::exit(1),
    };
    let request: DecodeRequest = match serde_json::from_slice(&request_bytes) {
        Ok(r) => r,
        Err(e) => {
            let resp = DecodeResponse::Error(DecodeError::DecodeFailed {
                reason: format!("malformed request: {e}"),
            });
            let _ = write_frame(&mut stdout, &serde_json::to_vec(&resp).unwrap());
            std::process::exit(2);
        }
    };

    let response = match request {
        DecodeRequest::Image { bytes, mime_hint } => decode_image(bytes, &mime_hint),
        DecodeRequest::Pdf { bytes } => decode_pdf(bytes),
    };

    let _ = write_frame(&mut stdout, &serde_json::to_vec(&response).unwrap());
}

fn decode_image(bytes: Vec<u8>, _mime_hint: &str) -> DecodeResponse {
    let img = match image::load_from_memory(&bytes) {
        Ok(img) => img,
        Err(e) => {
            return DecodeResponse::Error(DecodeError::DecodeFailed {
                reason: format!("image::load_from_memory: {e}"),
            });
        }
    };
    // Re-encode as PNG sRGB 8-bit. EXIF / XMP / iCCP / thumbnails are
    // dropped by re-encoding from the decoded RGBA buffer rather than
    // passing through the original container.
    let mut out = Vec::with_capacity(bytes.len());
    if let Err(e) = img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png) {
        return DecodeResponse::Error(DecodeError::DecodeFailed {
            reason: format!("re-encode: {e}"),
        });
    }
    DecodeResponse::Ok {
        png_bytes: out,
        decoder_version: DECODER_VERSION.into(),
    }
}

fn decode_pdf(_bytes: Vec<u8>) -> DecodeResponse {
    // Real PDF body lands in M3.4. Stub here so M3.2.2's bin compiles
    // and the protocol round-trip can be tested independently.
    DecodeResponse::Error(DecodeError::DecodeFailed {
        reason: "PDF decode not implemented in M3.2; lands in M3.4".into(),
    })
}
