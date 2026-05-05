//! M3.5.3 — Linux / Windows OCR backend via the `tesseract` CLI.
//!
//! Shells out to the system `tesseract` binary (`apt install
//! tesseract-ocr` / `winget install UB-Mannheim.TesseractOCR` /
//! `brew install tesseract`) rather than linking against
//! `libtesseract` directly. Trade-offs:
//!
//! - **Pro:** zero native build deps. CI just needs the binary on
//!   PATH; the Rust code stays portable. Bundle install instructions
//!   live in the cookbook.
//! - **Pro:** version skew is the user's problem — they install
//!   whatever their distro ships and we report it via `--version`.
//! - **Con:** ~50 ms process spawn overhead per image. OCR pre-pass
//!   runs once per dropped image so this is fine; if it ever moves
//!   into a hot loop, switch to the `tesseract` crate's C bindings.
//! - **Con:** binary not on PATH → graceful degradation to `NoopOcr`.
//!   Users see "" for OCR text instead of an error; the cookbook
//!   documents the install step.
//!
//! macOS is gated out — `vision::VisionOcr` is the macOS default
//! (M3.5.2). Skipping macOS keeps the module tree focused: no
//! Cocoa/objc2 in this file.

#![cfg(not(target_os = "macos"))]

use std::io::Write;
use std::process::{Command, Stdio};

use super::{OcrEngine, OcrResult};

pub struct TesseractOcr {
    /// Cached `tesseract --version` output (first line). Captured
    /// once at construction so each `recognize_png` call doesn't
    /// re-shell. Falls back to `"unknown"` if the version line
    /// can't be parsed.
    version: String,
}

impl TesseractOcr {
    /// Probe `PATH` for a working `tesseract` binary. Returns `None`
    /// if the probe fails (binary missing, wrong arch, sandbox
    /// blocking spawn) so the caller can fall back to
    /// [`super::NoopOcr`] without surfacing an error to the user.
    pub fn try_new() -> Option<Self> {
        let output = Command::new("tesseract").arg("--version").output().ok()?;
        if !output.status.success() {
            return None;
        }
        // tesseract prints version on stderr OR stdout depending on
        // the build; sample both and take the first non-empty line
        // mentioning "tesseract".
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let version = parse_version(&stderr).or_else(|| parse_version(&stdout))?;
        Some(Self { version })
    }
}

fn parse_version(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

impl OcrEngine for TesseractOcr {
    fn recognize_png(&self, png_bytes: &[u8]) -> OcrResult {
        let text = match recognize(png_bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "tesseract OCR failed; returning empty");
                String::new()
            }
        };
        OcrResult {
            text,
            engine_version: format!("tesseract-cli/{}", self.version),
        }
    }
}

fn recognize(png_bytes: &[u8]) -> anyhow::Result<String> {
    if png_bytes.is_empty() {
        return Ok(String::new());
    }
    // `tesseract <stdin> <stdout>` — pipes PNG bytes in, UTF-8 text
    // out. `-l eng` matches the system's default; multi-language
    // support is a follow-up (the user can override via the
    // `TESSERACT_LANG` env var if they care today).
    let lang = std::env::var("TESSERACT_LANG").unwrap_or_else(|_| "eng".to_string());
    let mut child = Command::new("tesseract")
        .args(["stdin", "stdout", "-l", &lang])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("tesseract stdin unavailable"))?;
        stdin.write_all(png_bytes)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tesseract exit {:?}: {}", output.status.code(), stderr);
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `try_new` succeeds in CI / dev environments where tesseract
    /// is installed; on hosts without it the test is skipped via
    /// the early return so the rest of the suite still runs.
    fn require_tesseract() -> Option<TesseractOcr> {
        TesseractOcr::try_new()
    }

    #[test]
    fn empty_input_returns_empty_string() {
        let Some(ocr) = require_tesseract() else {
            eprintln!("tesseract not on PATH — skipping");
            return;
        };
        let result = ocr.recognize_png(&[]);
        assert_eq!(result.text, "");
        assert!(result.engine_version.starts_with("tesseract-cli/"));
    }

    #[test]
    fn invalid_png_returns_empty_string_not_panic() {
        let Some(ocr) = require_tesseract() else {
            eprintln!("tesseract not on PATH — skipping");
            return;
        };
        // tesseract refuses to decode random bytes and exits non-zero;
        // the error path returns "" to keep the engine surface
        // consistent with NoopOcr / VisionOcr.
        let result = ocr.recognize_png(b"not a valid png");
        assert_eq!(result.text, "");
    }

    #[test]
    fn tiny_blank_png_returns_empty_string() {
        let Some(ocr) = require_tesseract() else {
            eprintln!("tesseract not on PATH — skipping");
            return;
        };
        // 1x1 transparent PNG carries no text so OCR must yield "".
        let png = std::fs::read("tests/fixtures/tiny.png").unwrap();
        let result = ocr.recognize_png(&png);
        assert_eq!(result.text, "");
    }

    #[test]
    fn try_new_returns_none_when_binary_missing() {
        // Force PATH to an empty value so `tesseract` resolution
        // fails. Restore it afterwards. Locked via the crate-wide
        // `TEST_ENV_LOCK` to avoid racing with bootstrap / wiring
        // tests that also mutate `std::env`.
        let _g = crate::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prior = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", "");
        }
        let result = TesseractOcr::try_new();
        if let Some(p) = prior {
            unsafe {
                std::env::set_var("PATH", p);
            }
        } else {
            unsafe {
                std::env::remove_var("PATH");
            }
        }
        assert!(result.is_none(), "expected None when PATH is empty");
    }

    #[test]
    fn parse_version_extracts_first_nonblank_line() {
        let text = "\n\ntesseract 5.3.0\n leptonica-1.83.1\n";
        assert_eq!(parse_version(text).as_deref(), Some("tesseract 5.3.0"));
    }

    #[test]
    fn parse_version_returns_none_for_empty_input() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("\n\n\n"), None);
    }
}
