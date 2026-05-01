use mur_agent_gui_lib::multimodal::ocr::{NoopOcr, OcrEngine};

#[test]
fn noop_returns_empty_string() {
    let e = NoopOcr;
    let r = e.recognize_png(&[0x89, 0x50, 0x4e, 0x47]);
    assert!(r.text.is_empty());
    assert!(r.engine_version.starts_with("noop"));
}

#[test]
fn default_engine_constructs() {
    // On every platform, default_engine() returns SOME engine — Vision
    // on macOS (when M3.5.2 ships), tesseract on Linux/Windows (when
    // M3.5.3 ships), or NoopOcr as the universal fallback today.
    let e = mur_agent_gui_lib::multimodal::ocr::default_engine();
    let r = e.recognize_png(&[0x89, 0x50, 0x4e, 0x47]);
    // For now Noop; later platform impls will return real text. Either
    // way, engine_version is non-empty.
    assert!(!r.engine_version.is_empty());
}
