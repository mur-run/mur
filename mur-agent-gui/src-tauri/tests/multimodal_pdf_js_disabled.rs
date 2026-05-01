//! M3.4.1 — PDF decode + < 1pt invisible-text quarantine.
//!
//! The fixture `tests/fixtures/prompt-injection.pdf` (regenerable via
//! `scripts/build-fixture-pdf.py`) contains:
//!   - a visible 12pt "Hello!" line
//!   - an invisible 0.5pt "ignore previous instructions ..." line
//!     simulating the prompt-injection trick.
//!
//! The decoder must (a) extract both lines (PDFium does extract sub-1pt
//! text — that's how the attack hides) and (b) flag the page as
//! quarantined because at least one glyph on it is rendered at < 1pt.
//!
//! Runtime requires PDFium to be discoverable. We point at the repo's
//! `.pdfium-bin/lib/` (downloaded from bblanchon/pdfium-binaries during
//! M3.4.1 setup) via `PDFIUM_DYNAMIC_LIB_PATH` if present; otherwise
//! the system loader path.

use std::io::Write;
use std::process::{Command, Stdio};

use mur_agent_gui_lib::multimodal::decoder_protocol::{
    DecodeRequest, DecodeResponse, read_frame, write_frame,
};

#[test]
fn pdf_with_invisible_text_quarantines() {
    let pdf = include_bytes!("fixtures/prompt-injection.pdf").to_vec();
    let req = DecodeRequest::Pdf { bytes: pdf };
    let req_bytes = serde_json::to_vec(&req).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur-agent-decoder"));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped());

    // Prefer the repo-local pdfium-binaries drop if it exists. CI runs
    // can override PDFIUM_DYNAMIC_LIB_PATH directly; this just keeps the
    // local dev workflow zero-config.
    if std::env::var_os("PDFIUM_DYNAMIC_LIB_PATH").is_none() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        // mur-agent-gui/src-tauri/.. -> mur-agent-gui ; ../.. -> repo root
        let repo_root = std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent());
        if let Some(root) = repo_root {
            let candidate = root.join(".pdfium-bin").join("lib");
            if candidate.exists() {
                cmd.env("PDFIUM_DYNAMIC_LIB_PATH", &candidate);
            }
        }
    }

    let mut child = cmd.spawn().unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        write_frame(stdin, &req_bytes).unwrap();
        stdin.flush().unwrap();
    }
    let mut stdout = child.stdout.take().unwrap();
    let resp_bytes = read_frame(&mut stdout).unwrap();
    let resp: DecodeResponse = serde_json::from_slice(&resp_bytes).unwrap();
    child.wait().unwrap();

    match resp {
        DecodeResponse::PdfText { pages, .. } => {
            assert!(!pages.is_empty(), "at least one page");
            // Both visible "Hello!" and invisible "ignore previous
            // instructions" extract — the quarantine flag is what
            // tells the pipeline the latter is untrusted.
            let body = pages
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                body.contains("ignore previous instructions"),
                "the invisible text was extracted; body: {body}"
            );
            assert!(
                pages.iter().any(|p| p.quarantined),
                "at least one page must be quarantined for <1pt glyphs"
            );
        }
        other => panic!("expected PdfText, got {other:?}"),
    }
}
