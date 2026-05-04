//! Track C3 channel D — macOS dock-icon drop target.
//!
//! When the user drags a file onto the dock icon, macOS delivers an
//! `application:openFiles:` AppleEvent. Tauri 2 surfaces it as
//! [`tauri::RunEvent::Opened`] with a `Vec<url::Url>` of `file://`
//! URLs. We translate each URL to a path, run [`classify_path`] to
//! pick a [`ShareKind`] (image vs. generic file), wrap it in a
//! [`SharePayload`] tagged `source = "dock"`, and hand it to the
//! [`SendIngestor`].
//!
//! Production wiring (`tauri::Builder::run` callback that pumps
//! `RunEvent::Opened` through the ingestor) lands in a follow-up; the
//! harness drives the same seam through `MockApp::simulate_opened`
//! (M-c3.4.3).
//!
//! [`SendIngestor`]: super::SendIngestor

#![cfg(target_os = "macos")]

use std::path::Path;

use super::ShareKind;

/// Map a dropped path to the right [`ShareKind`] based on its
/// extension. Image-y extensions (PNG/JPEG/GIF/WebP/HEIC/HEIF) route
/// to [`ShareKind::Image`] so the multimodal pipeline runs OCR; every
/// other extension (PDF, txt, md, .docx, …) goes to
/// [`ShareKind::File`] which `process_artifact` dispatches by mime
/// type. Extension matching is case-insensitive.
pub fn classify_path(p: &Path) -> ShareKind {
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif") => {
            ShareKind::Image(p.to_path_buf())
        }
        _ => ShareKind::File(p.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn classify_png_as_image() {
        assert!(matches!(
            classify_path(&PathBuf::from("/tmp/foo.png")),
            ShareKind::Image(_)
        ));
    }

    #[test]
    fn classify_pdf_as_file() {
        assert!(matches!(
            classify_path(&PathBuf::from("/tmp/foo.pdf")),
            ShareKind::File(_)
        ));
    }

    #[test]
    fn classify_uppercase_extension_is_case_insensitive() {
        assert!(matches!(
            classify_path(&PathBuf::from("/tmp/SCREENSHOT.JPEG")),
            ShareKind::Image(_)
        ));
    }

    #[test]
    fn classify_no_extension_falls_back_to_file() {
        assert!(matches!(
            classify_path(&PathBuf::from("/tmp/README")),
            ShareKind::File(_)
        ));
    }
}
