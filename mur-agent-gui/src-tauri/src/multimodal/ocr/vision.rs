//! M3.5.2 — macOS Vision.framework OCR backend.
//!
//! Wraps `VNRecognizeTextRequest` + `VNImageRequestHandler`. The handler
//! is created with `initWithData:options:` so we hand it the PNG bytes
//! directly without bouncing through a `CGImage` (Vision accepts any
//! image format Core Image can decode, which covers PNG / JPEG / HEIC
//! / BMP / GIF for our purposes).
//!
//! Recognition level is `Accurate` (Apple's neural-net path); language
//! correction is on; languages defaults to system preference order
//! ("en-US" first if no locale hint). On-device — no network egress,
//! satisfying B0 rule 14's "local OCR pre-pass" requirement.
//!
//! All work happens synchronously on the calling thread. Vision's
//! `performRequests:error:` blocks until results are populated, which
//! is the contract `OcrEngine::recognize_png` promises.

#![cfg(target_os = "macos")]

use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation, VNRequest,
    VNRequestTextRecognitionLevel,
};

use super::{OcrEngine, OcrResult};

pub struct VisionOcr;

impl VisionOcr {
    pub fn new() -> Self {
        Self
    }
}

impl Default for VisionOcr {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrEngine for VisionOcr {
    fn recognize_png(&self, png_bytes: &[u8]) -> OcrResult {
        // SAFETY: Vision is documented to be callable from any thread;
        // the underlying ML pipelines run off the calling thread
        // internally. The only main-thread requirement on Cocoa is
        // for UI; Vision is a model-evaluation framework.
        let text = match recognize(png_bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "Vision OCR failed; returning empty");
                String::new()
            }
        };
        OcrResult {
            text,
            engine_version: "macos-vision/14".into(),
        }
    }
}

fn recognize(png_bytes: &[u8]) -> anyhow::Result<String> {
    if png_bytes.is_empty() {
        return Ok(String::new());
    }

    let data: Retained<NSData> = NSData::with_bytes(png_bytes);
    // Empty options dict. `VNImageOption` is `pub type … = NSString`,
    // so `NSDictionary<NSString, AnyObject>` matches Vision's
    // `&NSDictionary<VNImageOption, AnyObject>` parameter directly —
    // no transmute needed.
    let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &data,
        &options,
    );

    let request = VNRecognizeTextRequest::new();
    // Accurate (slower, ML-based) — quality matters more than latency
    // for an OCR pre-pass that runs once per dropped image.
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    // Language correction smooths over OCR misreads. Cheap to leave on.
    request.setUsesLanguageCorrection(true);

    // Cast `&VNRecognizeTextRequest` → `&VNRequest` (parent class) so
    // `NSArray::from_slice` accepts it. objc2 newtypes expose this via
    // `Deref` conformance through the `super(...)` declaration.
    let request_as_base: &VNRequest = &request;
    let requests = NSArray::from_slice(&[request_as_base]);

    handler
        .performRequests_error(&requests)
        .map_err(|e| anyhow::anyhow!("Vision performRequests failed: {e:?}"))?;

    let Some(observations) = request.results() else {
        return Ok(String::new());
    };

    // Each observation may carry multiple candidates (Vision ranks by
    // confidence). Take the top candidate per observation; concatenate
    // with newlines so block layout is preserved roughly.
    let mut out = String::new();
    for obs in observations.iter() {
        // Downcast `&VNObservation` → `&VNRecognizedTextObservation`.
        // The OCR request only ever populates this concrete type, so
        // the cast is sound; objc2's `downcast_ref` validates at
        // runtime via isKindOfClass.
        let Some(text_obs) = obs.downcast_ref::<VNRecognizedTextObservation>() else {
            continue;
        };
        let candidates = text_obs.topCandidates(1);
        if let Some(top) = candidates.iter().next() {
            let s = top.string();
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&s.to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_string() {
        let ocr = VisionOcr::new();
        let result = ocr.recognize_png(&[]);
        assert_eq!(result.text, "");
        assert!(result.engine_version.starts_with("macos-vision/"));
    }

    #[test]
    fn invalid_png_returns_empty_string_not_panic() {
        let ocr = VisionOcr::new();
        // Random bytes — Vision should fail to decode and we should
        // surface "" rather than panicking.
        let result = ocr.recognize_png(b"not a valid png");
        assert_eq!(result.text, "");
    }

    #[test]
    fn tiny_blank_png_returns_empty_string() {
        let ocr = VisionOcr::new();
        // Standard 1x1 PNG fixture from Track C3 — has no text so OCR
        // should return empty. Decoding succeeds; just no text found.
        let png = std::fs::read("tests/fixtures/tiny.png").unwrap();
        let result = ocr.recognize_png(&png);
        assert_eq!(result.text, "");
    }
}
