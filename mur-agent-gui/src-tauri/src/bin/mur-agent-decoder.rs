//! `mur-agent-decoder` — sandboxed image/PDF decoder subprocess.
//!
//! Reads exactly one `DecodeRequest` frame from stdin, runs the
//! appropriate decode, writes one `DecodeResponse` frame to stdout,
//! exits. Process isolation is the only mitigation in M3.2.2: a
//! malicious image that exploits libpng / libheif crashes only this
//! process, not the GUI. Real OS-level sandbox (macOS SBPL + Landlock)
//! lands in B1 (v2).
//!
//! ## PDF decode (M3.4.1)
//!
//! Backed by `pdfium-render` 0.8 against a dynamically loaded PDFium
//! shared library (`sync` feature only — no static link). At runtime
//! the library is discovered in this order:
//!
//! 1. `PDFIUM_DYNAMIC_LIB_PATH` (env var) — fully qualified path used
//!    in dev / CI. Set this to the directory containing
//!    `libpdfium.dylib` / `libpdfium.so` / `pdfium.dll`.
//! 2. `Pdfium::bind_to_system_library()` — looks for the platform name
//!    (`libpdfium.dylib` on macOS, `libpdfium.so` on Linux,
//!    `pdfium.dll` on Windows) on the system loader path
//!    (`DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` / `PATH`).
//!
//! If both fail we surface a `DecodeFailed` error rather than panicking.
//! `Pdfium::default()` would panic, which is unacceptable for a
//! subprocess that the GUI needs structured errors back from.
//!
//! PDFium does not execute embedded PDF JavaScript in this binding (we
//! never call `FORM_DoDocumentJSAction`). Doc-level catalog hardening
//! (drop `/JS`, `/EmbeddedFile`, `/Launch`, `/RichMedia`, `/SubmitForm`)
//! requires direct catalog dictionary access which `pdfium-render` 0.8
//! doesn't expose; we rely on default-no-JS plus the < 1pt glyph
//! quarantine flag to catch invisible-text injection.

use std::io;

use mur_agent_gui_lib::multimodal::decoder_protocol::{
    DecodeError, DecodeRequest, DecodeResponse, PdfPageText, read_frame, write_frame,
};

const DECODER_VERSION: &str = concat!("image-rs/0.25 (host=", env!("CARGO_PKG_VERSION"), ")",);
const PDF_DECODER_VERSION: &str =
    concat!("pdfium-render/0.8 (host=", env!("CARGO_PKG_VERSION"), ")");

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

fn decode_pdf(bytes: Vec<u8>) -> DecodeResponse {
    // Avoid glob-importing the prelude because pdfium-render also
    // exports a type literally called `PdfPageText` which would shadow
    // our protocol struct of the same name (used for the decoder
    // response).
    use pdfium_render::prelude::Pdfium;

    // Bindings discovery — PDFIUM_DYNAMIC_LIB_PATH first, then system
    // loader path. Avoids `Pdfium::default()` because that panics when
    // the lib is missing; we want a structured DecodeError back.
    let bindings = if let Ok(path) = std::env::var("PDFIUM_DYNAMIC_LIB_PATH") {
        let lib = std::path::Path::new(&path).join(Pdfium::pdfium_platform_library_name());
        Pdfium::bind_to_library(&lib)
            .or_else(|_| Pdfium::bind_to_library(&path))
            .or_else(|_| Pdfium::bind_to_system_library())
    } else {
        Pdfium::bind_to_system_library()
    };
    let bindings = match bindings {
        Ok(b) => b,
        Err(e) => {
            return DecodeResponse::Error(DecodeError::DecodeFailed {
                reason: format!(
                    "pdfium bind: {e}; install libpdfium and/or set \
                     PDFIUM_DYNAMIC_LIB_PATH to its directory"
                ),
            });
        }
    };
    let pdfium = Pdfium::new(bindings);

    let doc = match pdfium.load_pdf_from_byte_slice(&bytes, None) {
        Ok(d) => d,
        Err(e) => {
            return DecodeResponse::Error(DecodeError::DecodeFailed {
                reason: format!("pdfium load: {e}"),
            });
        }
    };

    let mut pages: Vec<PdfPageText> = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        let text_obj = match page.text() {
            Ok(t) => t,
            Err(_) => {
                // Page has no text layer (e.g. pure scan). Emit an empty
                // text entry so the page index is still represented; the
                // OCR path (M3.5) is responsible for those.
                pages.push(PdfPageText {
                    page: (i + 1) as u32,
                    text: String::new(),
                    quarantined: false,
                });
                continue;
            }
        };

        let raw = text_obj.all();

        // Quarantine flag: any glyph rendered at < 1pt. PDFium does
        // extract these glyphs (their width in points reflects the
        // tiny font size, but the unicode is preserved), so the
        // injection text is in `raw` — we just tag the page so the
        // pipeline can wrap it as untrusted.
        let mut quarantined = false;
        let chars = text_obj.chars();
        for ch in chars.iter() {
            if ch.unscaled_font_size().value < 1.0 {
                quarantined = true;
                break;
            }
        }

        pages.push(PdfPageText {
            page: (i + 1) as u32,
            text: raw,
            quarantined,
        });
    }

    DecodeResponse::PdfText {
        pages,
        decoder_version: PDF_DECODER_VERSION.into(),
    }
}
