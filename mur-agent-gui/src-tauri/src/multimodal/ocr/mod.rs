//! OCR engine abstraction.
//!
//! `OcrEngine` is the trait every platform-specific impl satisfies.
//! `default_engine()` returns the appropriate platform default:
//!
//! * macOS (when M3.5.2 lands): `VisionOcr` via `objc2-vision`
//! * Linux/Windows (when M3.5.3 lands): `TesseractOcr` shelling to the
//!   `tesseract` CLI binary
//! * Today (M3.5.1): `NoopOcr` everywhere — returns empty string. The
//!   pipeline still runs; image artifacts ship with `ocr_text:
//!   Some("")` until M3.5.2 / M3.5.3 wire real engines.
//!
//! This means the rest of M3 (UI, Tauri commands, B0SafetyHook) can
//! land without waiting on the native-OCR work.

pub mod platform;

pub struct OcrResult {
    pub text: String,
    pub engine_version: String,
}

pub trait OcrEngine: Send + Sync {
    fn recognize_png(&self, png_bytes: &[u8]) -> OcrResult;
}

/// Universal fallback. Returns empty string. Used in tests + on hosts
/// that don't yet have a real engine wired up.
pub struct NoopOcr;

impl OcrEngine for NoopOcr {
    fn recognize_png(&self, _bytes: &[u8]) -> OcrResult {
        OcrResult {
            text: String::new(),
            engine_version: "noop/1.0".into(),
        }
    }
}

/// Build the platform-default OCR engine. M3.5.1 always returns
/// `NoopOcr`; M3.5.2 + M3.5.3 will replace this with real platform
/// dispatches.
pub fn default_engine() -> Box<dyn OcrEngine> {
    platform::default_engine()
}
