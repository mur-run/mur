//! Platform dispatch for `default_engine()`.
//!
//! M3.5.1 returns `NoopOcr` on every platform. When M3.5.2 ships the
//! macOS Vision.framework backend, replace the macOS arm; when M3.5.3
//! ships tesseract, replace the non-macOS arm.

#[cfg(target_os = "macos")]
mod imp {
    pub fn default_engine() -> Box<dyn super::super::OcrEngine> {
        // M3.5.2 — Vision.framework backend, on-device + zero-network.
        Box::new(super::super::vision::VisionOcr::new())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn default_engine() -> Box<dyn super::super::OcrEngine> {
        // TODO(M3.5.3): try TesseractOcr::new() and fall back to NoopOcr
        // if the tesseract binary isn't on PATH.
        Box::new(super::super::NoopOcr)
    }
}

pub use imp::default_engine;
